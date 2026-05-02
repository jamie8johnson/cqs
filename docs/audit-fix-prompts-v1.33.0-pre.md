# v1.30.1 Audit Fix Prompts

Generated: 2026-04-28T19:08:55Z

# v1.30.1 Audit — P1 Fix Prompts

10 fix prompts covering the 14 P1 findings (after grouping). All line citations verified against current source 2026-04-28.

Group map (audit IDs → prompt #):
- P1.1: CQ-V1.30.1-2 + DS-V1.30.1-D6 + TC-ADV-1.30.1-8 (single state-machine + test fix)
- P1.2: CQ-V1.30.1-1 + AC-V1.30.1-4 + DS-V1.30.1-D8 (single reset-ordering fix)
- P1.3: CQ-V1.30.1-4 + AC-V1.30.1-5 (auth ladder rewrite — strip ordering + case-fold)
- P1.4: DOC-V1.30.1-1 (PRIVACY/SECURITY cache key)
- P1.5: SEC-V1.30.1-2 (symlink-behaviour matrix)
- P1.6: SEC-V1.30.1-1 (read --focus / context trust_level — depends on type extension)
- P1.7: DOC-V1.30.1-7 (SECURITY auth surface backfill)
- P1.8: DOC-V1.30.1-4 (ROADMAP #1182 closed)
- P1.9: SHL-V1.30-1 (`embed_batch_size_for` wiring — 2 production sites)
- P1.10: SEC-V1.30.1-8 (env snapshot redaction)
- P1.11: DS-V1.30.1-D2 (reconcile cap)
- P1.12: AC-V1.30.1-1 (reconcile non-monotonic mtime)

Total finding IDs covered: 14. Total distinct prompts: 12 (some grouped).

---

## P1.1: CQ-V1.30.1-2 + DS-V1.30.1-D6 + TC-ADV-1.30.1-8 — `delta_saturated` ignored by `compute()`

**Files:** `src/watch_status.rs:199-209`, `src/watch_status.rs:233-323` (test module)
**Effort:** ~10 minutes
**Why:** `WatchSnapshotInput` carries `delta_saturated`, and `publish_watch_snapshot` plumbs it through, but `compute()` never consults the field. After a saturated rebuild discards on swap, `pending_rebuild` becomes `None` and `compute()` reports `Fresh` — `cqs eval --require-fresh` accepts a doomed rebuild's stale on-disk index. Defeats #1182's day-1 freshness contract. Same root cause cross-listed in code-quality, data-safety, and adversarial test coverage.

### Current code

```rust
//   src/watch_status.rs:199-209
    pub fn compute(input: WatchSnapshotInput<'_>) -> Self {
        let state = if input.rebuild_in_flight {
            FreshnessState::Rebuilding
        } else if input.pending_files_count > 0
            || input.pending_notes
            || input.dropped_this_cycle > 0
        {
            FreshnessState::Stale
        } else {
            FreshnessState::Fresh
        };
```

### Replacement

```rust
    pub fn compute(input: WatchSnapshotInput<'_>) -> Self {
        let state = if input.rebuild_in_flight {
            FreshnessState::Rebuilding
        } else if input.pending_files_count > 0
            || input.pending_notes
            || input.dropped_this_cycle > 0
            || input.delta_saturated
        {
            // CQ-V1.30.1-2 / DS-V1.30.1-D6: a saturated delta means the
            // rebuilt HNSW was discarded on swap (rebuild.rs:60-63); the
            // on-disk index is whatever was there before the rebuild
            // started. Treat as Stale until the next threshold rebuild
            // lands cleanly, so `cqs eval --require-fresh` waits.
            FreshnessState::Stale
        } else {
            FreshnessState::Fresh
        };
```

Then add the missing test in the same file's `#[cfg(test)] mod tests` block, right after the `dropped_events_mark_stale` test (around line 295):

```rust
    /// CQ-V1.30.1-2 / TC-ADV-1.30.1-8: a saturated delta means the in-flight
    /// rebuild's pending delta exceeded `MAX_PENDING_REBUILD_DELTA` and the
    /// rebuilt HNSW will be discarded on swap. Until the next threshold
    /// rebuild reads SQLite fresh, the on-disk index is stale. The flag
    /// is published; `compute()` must treat it as a Stale signal so
    /// `cqs eval --require-fresh` doesn't accept a doomed rebuild.
    #[test]
    fn delta_saturated_marks_stale_when_no_other_work() {
        let snap = WatchSnapshot::compute(WatchSnapshotInput {
            pending_files_count: 0,
            pending_notes: false,
            rebuild_in_flight: false,
            delta_saturated: true,
            incremental_count: 0,
            dropped_this_cycle: 0,
            last_event: std::time::Instant::now(),
            last_synced_at: None,
            _marker: std::marker::PhantomData,
        });
        assert_eq!(snap.state, FreshnessState::Stale);
        assert!(snap.delta_saturated);
    }

    /// `Rebuilding` still wins when the rebuild is in flight even with a
    /// saturated delta — the saturation will be observed when the rebuild
    /// drains and `rebuild_in_flight` flips to false.
    #[test]
    fn rebuild_in_flight_dominates_over_delta_saturated() {
        let snap = WatchSnapshot::compute(WatchSnapshotInput {
            pending_files_count: 0,
            pending_notes: false,
            rebuild_in_flight: true,
            delta_saturated: true,
            incremental_count: 0,
            dropped_this_cycle: 0,
            last_event: std::time::Instant::now(),
            last_synced_at: None,
            _marker: std::marker::PhantomData,
        });
        assert_eq!(snap.state, FreshnessState::Rebuilding);
    }
```

### Verification

- `cargo build --features cuda-index`
- `cargo test --features cuda-index --lib watch_status::tests::delta_saturated_marks_stale_when_no_other_work`
- `cargo test --features cuda-index --lib watch_status::tests::rebuild_in_flight_dominates_over_delta_saturated`
- `cargo test --features cuda-index --lib watch_status::tests` (full suite — empty-state-fresh, dropped-marks-stale, rebuild-dominates must still pass)

---

## P1.2: CQ-V1.30.1-1 + AC-V1.30.1-4 + DS-V1.30.1-D8 — `dropped_this_cycle` reset before publish

**Files:** `src/cli/watch/events.rs:131-157`
**Effort:** ~15 minutes
**Why:** `process_file_changes` clears `state.dropped_this_cycle = 0` at line 145 *before* (a) the embedder-init check that may early-`return` and (b) the snapshot publish that runs at the end of the outer loop iteration. Two failure modes share one fix:

1. (CQ-V1.30.1-1 / DS-V1.30.1-D8) The next `publish_watch_snapshot` always sees `dropped_this_cycle == 0` even when the cycle that just ran started with a non-zero count. `compute()` uses `dropped_this_cycle > 0` as a Stale signal — never observed.
2. (AC-V1.30.1-4) When `try_init_embedder` returns `None` at line 156, the function early-returns with `pending_files` already drained AND `dropped_this_cycle` zeroed, having processed no chunks. Total signal loss when embedder init fails.

Fix: move the reset to *after* the embedder-init check, then keep the warn unconditional so operators see the count even if drain fails downstream.

### Current code

```rust
//   src/cli/watch/events.rs:131-157
pub(super) fn process_file_changes(cfg: &WatchConfig, store: &Store, state: &mut WatchState) {
    let files: Vec<PathBuf> = state.pending_files.drain().collect();
    let _span = info_span!("process_file_changes", file_count = files.len()).entered();
    state.pending_files.shrink_to(64);

    // RM-V1.25-23: surface truncated cycles at warn level so operators
    // notice the gap. The per-event drops are logged at debug to keep
    // the journal clean on bulk edits.
    if state.dropped_this_cycle > 0 {
        tracing::warn!(
            dropped = state.dropped_this_cycle,
            cap = max_pending_files(),
            "Watch event queue full this cycle; dropping events. Run `cqs index` to catch up"
        );
        state.dropped_this_cycle = 0;
    }
    if !cfg.quiet {
        println!("\n{} file(s) changed, reindexing...", files.len());
        for f in &files {
            println!("  {}", f.display());
        }
    }

    let emb = match try_init_embedder(cfg.embedder, &mut state.embedder_backoff, cfg.model_config) {
        Some(e) => e,
        None => return,
    };
```

### Replacement

```rust
pub(super) fn process_file_changes(cfg: &WatchConfig, store: &Store, state: &mut WatchState) {
    let files: Vec<PathBuf> = state.pending_files.drain().collect();
    let _span = info_span!("process_file_changes", file_count = files.len()).entered();
    state.pending_files.shrink_to(64);

    // CQ-V1.30.1-1 / AC-V1.30.1-4 / DS-V1.30.1-D8: warn at the top so
    // operators see the count, but DO NOT reset the counter here — the
    // outer loop's `publish_watch_snapshot` runs after this function
    // returns, and `WatchSnapshot::compute` uses `dropped_this_cycle > 0`
    // as a Stale signal. If we zero it before the embedder check below
    // (which may early-return on init failure), the snapshot reports
    // `Fresh` even though events were dropped and never reindexed —
    // defeating `cqs eval --require-fresh`. Reset only after a
    // successful drain so the next cycle's snapshot reflects the
    // truthful state.
    if state.dropped_this_cycle > 0 {
        tracing::warn!(
            dropped = state.dropped_this_cycle,
            cap = max_pending_files(),
            "Watch event queue full this cycle; dropping events. Run `cqs index` to catch up"
        );
    }
    if !cfg.quiet {
        println!("\n{} file(s) changed, reindexing...", files.len());
        for f in &files {
            println!("  {}", f.display());
        }
    }

    let emb = match try_init_embedder(cfg.embedder, &mut state.embedder_backoff, cfg.model_config) {
        Some(e) => e,
        None => return,
    };
```

Then, after the successful `reindex_files` `Ok(...)` arm completes (around line 195, inside the `Ok((count, content_hashes)) =>` block — see existing code at events.rs:195+), add the reset:

```rust
        Ok((count, content_hashes)) => {
            // Record mtimes to skip duplicate events
            for (file, mtime) in pre_mtimes {
                state.last_indexed_mtime.insert(file, mtime);
            }
            // CQ-V1.30.1-1 / AC-V1.30.1-4: reset only after a successful
            // drain. The dropped events surfaced in the warn above are
            // also queued for reconcile (Layer 2) on the next idle pass,
            // so the count stays meaningful exactly until the reconcile
            // refills `pending_files` with the same paths.
            state.dropped_this_cycle = 0;
            // ... (existing body continues: prune_last_indexed_mtime, splade encoding, HNSW maintenance)
```

### Verification

- `cargo build --features cuda-index`
- `cargo test --features cuda-index --lib watch::events`
- Add a regression test (in `src/cli/watch/tests.rs` or `events.rs` `#[cfg(test)]` block) that:
  - Seeds `state.dropped_this_cycle = 5` and `state.pending_files` with one path
  - Calls `process_file_changes` against a fixture with no embedder available (so the early-return path fires)
  - Asserts `state.dropped_this_cycle == 5` after the call (NOT zeroed)
- Manual: `cqs status --watch-fresh --json` after a 200-file save burst that exceeds `CQS_WATCH_MAX_PENDING=10` — must report `state: "stale"` until reconcile drains.

---

## P1.3: CQ-V1.30.1-4 + AC-V1.30.1-5 — auth ladder leaks `?token=` via cookie-wins-first + case-sensitive parser

**Files:** `src/serve/auth.rs:243-321`, `src/serve/auth.rs:572-600` (tests to invert)
**Effort:** ~25 minutes
**Why:** Two cooperating bugs in the SEC-7 token-redirect path:

1. (AC-V1.30.1-5) `check_request` checks Bearer → cookie → query in that order. A request with both a valid cookie *and* `?token=<…>` returns `AuthOutcome::Ok` (not `OkViaQueryParam`), so the redirect at line 360 never fires — the token sits in the URL bar permanently after a bookmarked-URL reload.
2. (CQ-V1.30.1-4) `strip_token_param` and `check_request` do byte-literal `starts_with("token=")` matches; `?Token=<token>` (capital `T`) and `?%74oken=<token>` (percent-encoded) are not recognised, so the redirect path never strips them either. The current tests at line 572-600 *pin the bug* with comments calling out the SEC-7 leakage.

Single fix: lowercase + percent-decode the parameter key once during the URI walk, AND treat presence of `?token=…` as redirect-eligible regardless of which channel matched (sniff after Ok). Then invert the two pinned tests.

### Current code (the parsing helpers)

```rust
//   src/serve/auth.rs:243-321
/// Strip the `token` parameter from the URI's query string for the
/// post-auth redirect. Other query params are preserved in their
/// original order.
fn strip_token_param(uri: &Uri) -> String {
    let path = uri.path();
    let Some(query) = uri.query() else {
        return path.to_string();
    };
    let kept: Vec<&str> = query
        .split('&')
        .filter(|pair| !pair.starts_with("token=") && *pair != "token")
        .collect();
    if kept.is_empty() {
        path.to_string()
    } else {
        format!("{path}?{}", kept.join("&"))
    }
}

/// Extract the token from one of three channels — header, cookie,
/// or query string — and constant-time-compare against the launched
/// token. Returns `None` if none matched (caller emits 401).
///
/// `query_param_used` is set to true when the match came from
/// `?token=<…>`; the caller then sets a cookie and 302-redirects to
/// the clean URL.
fn check_request(req: &Request, expected: &AuthToken, cookie_name: &str) -> AuthOutcome {
    // 1. Authorization: Bearer …
    if let Some(bearer) = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    {
        if ct_eq(bearer, expected.as_str()) {
            return AuthOutcome::Ok;
        }
    }

    // 2. cqs_token_<port> cookie. RFC 6265 cookie syntax is name=value
    // pairs separated by `; `. We don't bother with quoted values —
    // the server only ever sets this cookie itself and never quotes
    // it. Cookie name is per-port (#1135) so two cqs serve instances
    // on the same host don't collide in the browser jar.
    if let Some(cookie_header) = req
        .headers()
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
    {
        let needle = format!("{cookie_name}=");
        for pair in cookie_header.split(';') {
            if let Some(value) = pair.trim().strip_prefix(&needle) {
                if ct_eq(value, expected.as_str()) {
                    return AuthOutcome::Ok;
                }
            }
        }
    }

    // 3. ?token=… query param. axum's `Query` extractor only deserializes
    // a typed struct; we want raw access without forcing every request
    // path through a fixed type, so we parse the URI's `query()` directly.
    if let Some(query) = req.uri().query() {
        for pair in query.split('&') {
            if let Some(value) = pair.strip_prefix("token=") {
                // URI query values can be percent-encoded; the token
                // alphabet is URL-safe base64 (`A-Z a-z 0-9 - _`) so no
                // percent-encoding is ever needed in practice. Compare
                // verbatim — a percent-encoded match would fail `ct_eq`,
                // which is the conservative choice.
                if ct_eq(value, expected.as_str()) {
                    return AuthOutcome::OkViaQueryParam;
                }
            }
        }
    }

    AuthOutcome::Unauthorized
}
```

### Replacement

```rust
/// Case-fold + percent-decode a query-pair key for comparison.
/// SEC-7 leakage fix (CQ-V1.30.1-4): `?Token=…` and `?%74oken=…` must be
/// recognised as `token=` so the redirect strips them.
fn pair_key_is_token(pair: &str) -> bool {
    let Some(eq_idx) = pair.find('=') else {
        return pair.eq_ignore_ascii_case("token");
    };
    let raw_key = &pair[..eq_idx];
    // Percent-decode the key. The token alphabet itself is URL-safe
    // base64 (no percent-encoding needed), but operators sometimes
    // hand-encode the key; we want `%74oken=` to match too.
    let decoded = percent_encoding::percent_decode_str(raw_key)
        .decode_utf8_lossy();
    decoded.eq_ignore_ascii_case("token")
}

/// Strip the `token` parameter from the URI's query string for the
/// post-auth redirect. Other query params are preserved in their
/// original order. Recognises `Token=`, `%74oken=`, and any case-/
/// percent-folded variant.
fn strip_token_param(uri: &Uri) -> String {
    let path = uri.path();
    let Some(query) = uri.query() else {
        return path.to_string();
    };
    let kept: Vec<&str> = query
        .split('&')
        .filter(|pair| !pair.is_empty() && !pair_key_is_token(pair))
        .collect();
    if kept.is_empty() {
        path.to_string()
    } else {
        format!("{path}?{}", kept.join("&"))
    }
}

/// Extract the token from one of three channels — header, cookie,
/// or query string — and constant-time-compare against the launched
/// token. AC-V1.30.1-5: even when Bearer or cookie matches, if a
/// `token=` query param is also present, return `OkViaQueryParam` so
/// the caller redirects to the clean URL — leaving a stale `?token=`
/// in the URL bar is the exact SEC-7 leakage path the redirect closes.
fn check_request(req: &Request, expected: &AuthToken, cookie_name: &str) -> AuthOutcome {
    // Sniff for any `?token=…` first — if present, we want to redirect
    // even when another channel also matches. Validity of the query
    // value isn't required for the redirect; the redirect's only job
    // is to scrub the URL bar.
    let query_has_token_param = req
        .uri()
        .query()
        .is_some_and(|q| q.split('&').any(pair_key_is_token));

    // 1. Authorization: Bearer …
    let bearer_ok = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|bearer| ct_eq(bearer, expected.as_str()));

    // 2. cqs_token_<port> cookie. RFC 6265 cookie syntax is name=value
    // pairs separated by `; `. We don't bother with quoted values —
    // the server only ever sets this cookie itself and never quotes
    // it. Cookie name is per-port (#1135) so two cqs serve instances
    // on the same host don't collide in the browser jar.
    let cookie_ok = req
        .headers()
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(|cookie_header| {
            let needle = format!("{cookie_name}=");
            cookie_header.split(';').any(|pair| {
                pair.trim()
                    .strip_prefix(&needle)
                    .is_some_and(|value| ct_eq(value, expected.as_str()))
            })
        })
        .unwrap_or(false);

    // 3. ?token=… query param. axum's `Query` extractor only deserializes
    // a typed struct; we want raw access without forcing every request
    // path through a fixed type, so we parse the URI's `query()` directly.
    let query_ok = req.uri().query().is_some_and(|query| {
        query
            .split('&')
            .filter(|pair| pair_key_is_token(pair))
            .any(|pair| {
                let value = pair.split_once('=').map(|(_, v)| v).unwrap_or("");
                ct_eq(value, expected.as_str())
            })
    });

    if !(bearer_ok || cookie_ok || query_ok) {
        return AuthOutcome::Unauthorized;
    }

    // AC-V1.30.1-5: presence of `?token=…` (any case-folded form) on a
    // request that authenticates by ANY channel must trigger the
    // redirect — otherwise the token sits in the URL bar permanently
    // after a bookmarked-URL reload, even when the cookie is what
    // matched.
    if query_has_token_param {
        AuthOutcome::OkViaQueryParam
    } else {
        AuthOutcome::Ok
    }
}
```

Add `percent-encoding = "2"` to `Cargo.toml`'s `[dependencies]` if not already there. (The crate is in the dependency tree via `reqwest`, but the import has to be explicit.) Verify with `cargo tree -p percent-encoding`.

Then invert the two pinned tests at `src/serve/auth.rs` (around lines 572-600):

```rust
    #[test]
    #[allow(non_snake_case)]
    fn p2_30_strip_token_param_capital_T_token_IS_stripped() {
        // CQ-V1.30.1-4: capital `Token=` is case-folded and stripped.
        // The SEC-7 leakage gap pinned by the previous test is closed.
        let uri: Uri = "/api/graph?Token=abc&depth=3".parse().unwrap();
        let stripped = strip_token_param(&uri);
        assert_eq!(stripped, "/api/graph?depth=3");
    }

    #[test]
    fn p2_30_strip_token_param_percent_encoded_key_IS_stripped() {
        // CQ-V1.30.1-4: `%74oken=` is percent-decoded and stripped.
        let uri: Uri = "/api/graph?%74oken=abc&depth=3".parse().unwrap();
        let stripped = strip_token_param(&uri);
        assert_eq!(stripped, "/api/graph?depth=3");
    }
```

Also add a new test for AC-V1.30.1-5 (cookie + redundant query → redirect):

```rust
    #[test]
    fn check_request_cookie_with_redundant_query_token_redirects() {
        // AC-V1.30.1-5: even when the cookie matches, a `?token=` query
        // param must trigger the redirect so the URL bar is scrubbed.
        let token = AuthToken::random();
        let cookie_name = "cqs_token_8080";
        let req = Request::builder()
            .uri("/api/graph?token=anything")
            .header(
                header::COOKIE,
                format!("{cookie_name}={}", token.as_str()),
            )
            .body(Body::empty())
            .unwrap();
        let outcome = check_request(&req, &token, cookie_name);
        assert!(matches!(outcome, AuthOutcome::OkViaQueryParam));
    }
```

### Verification

- `cargo build --features cuda-index`
- `cargo test --features cuda-index --lib serve::auth::tests::strip_token_param`
- `cargo test --features cuda-index --lib serve::auth::tests::p2_30_strip_token_param`
- `cargo test --features cuda-index --lib serve::auth::tests::check_request`
- Manual: `cqs serve --bind 127.0.0.1:8088 &` then `curl -i 'http://127.0.0.1:8088/?token=GOOD' --cookie 'cqs_token_8088=GOOD'` should return 302 to `/`.

---

## P1.4: DOC-V1.30.1-1 — PRIVACY/SECURITY misstate embedding-cache primary key

**Reframed during verification:** Original prompt invented a `purpose='summary'` cache row that doesn't exist — `embedding_cache` only has purposes `embedding` and `embedding_base`; the LLM summary text lives in a separate `llm_summaries` table.

**Files:** `PRIVACY.md:16`, `SECURITY.md:47`
**Effort:** ~5 minutes
**Why:** The cache schema at `src/cache.rs:263-278` is `PRIMARY KEY (content_hash, model_fingerprint, purpose)` — `purpose` was added in #1128 to discriminate the post-enrichment `embedding` column from the raw `embedding_base` column (added in v18). The two paths produce different vectors for the same content, and without `purpose` in the PK the second writer silently overwrites the first. PRIVACY.md tells users the key is a 2-tuple `(content_hash, model_id)`; SECURITY.md says the LLM summary is "cached by `content_hash`" without naming the table. Per "Docs Lying Is P1": both claims are wrong about *where and how* data is stored, and the corrected text must reflect actual schema (`CachePurpose` is exactly `Embedding ("embedding")` and `EmbeddingBase ("embedding_base")` per `src/cache.rs:84-104`; LLM summaries live in `llm_summaries` keyed by `(content_hash, purpose)` per `src/schema.sql:180-187`).

### Current docs

PRIVACY.md:16:
```markdown
- `.cqs/embeddings_cache.db` — per-project embedding cache, keyed by `(content_hash, model_id)` (#1105). Skips re-embedding chunks that haven't changed across reindexes / model swaps.
```

SECURITY.md:47:
```markdown
| **LLM-generated summaries** (`cqs index --llm-summaries`) | Claude is prompted with chunk content; a poisoned chunk can produce a summary that contains injection text. The summary is cached by `content_hash`, embedded, and replayed to downstream agents | Yes — cached in `llm_summaries` table |
```

### Replacement

PRIVACY.md:16:
```markdown
- `.cqs/embeddings_cache.db` — per-project embedding cache, keyed by `(content_hash, model_fingerprint, purpose)` (#1105, #1128). Skips re-embedding chunks that haven't changed across reindexes / model swaps; the `purpose` discriminator (`embedding` for the post-enrichment vector served by HNSW, `embedding_base` for the raw NL vector served by the dual-index "base" graph) prevents the two streams from overwriting each other when the same chunk produces both.
```

SECURITY.md:47:
```markdown
| **LLM-generated summaries** (`cqs index --llm-summaries`) | Claude is prompted with chunk content; a poisoned chunk can produce a summary that contains injection text. The summary text is cached in the `llm_summaries` table keyed by `(content_hash, purpose)` per `src/schema.sql:180-187`; the post-summary embedding flows through the normal `embeddings_cache.db` (purpose `embedding`, the same purpose served to search) and is replayed to downstream agents | Yes — cached in `llm_summaries` table + `embeddings_cache.db` |
```

### Verification

- `grep -n "model_id\|model_fingerprint\|purpose" /mnt/c/Projects/cqs/PRIVACY.md /mnt/c/Projects/cqs/SECURITY.md` — confirm the strings line up with `src/cache.rs:263-278` schema and `src/schema.sql:180-187`.
- `grep -n "CachePurpose\|as_str" /mnt/c/Projects/cqs/src/cache.rs | head` — confirm only `embedding` and `embedding_base` exist as purpose values; no `'summary'` purpose row exists in `embedding_cache`.
- No code build needed (doc-only).

---

## P1.5: SEC-V1.30.1-2 — SECURITY.md "Symlink Behavior" matrix contradicts indexer

**Files:** `SECURITY.md:203-215`, `SECURITY.md:162` (cross-check)
**Effort:** ~5 minutes
**Why:** SECURITY.md:203-215 promises symlinks are followed-then-validated (`project/link → project/src/file.rs` is allowed). Reality: `cqs::enumerate_files` at `src/lib.rs:601` calls `WalkBuilder::follow_links(false)` — every symlink is silently skipped, regardless of target. SECURITY.md:162 already correctly says "Symlinks are skipped during directory walks and archive extraction". The two sections cannot both be true. Operators expecting indexable in-tree symlinks (e.g. monorepo workspaces) get a partial index with no error message. Per "Docs Lying Is P1": the doc claim must match the code. The indexer behavior is the right one (avoids zip-slip-style attacks); the matrix needs the truth.

### Current docs (SECURITY.md:203-215)

```markdown
## Symlink Behavior

**Current behavior**: Symlinks are followed, then the resolved path is validated.

| Scenario | Behavior |
|----------|----------|
| `project/link → project/src/file.rs` | ✅ Allowed (target inside project) |
| `project/link → /etc/passwd` | ❌ Blocked (target outside project) |
| `project/link → ../sibling/file` | ❌ Blocked (target outside project) |

**TOCTOU consideration**: A symlink could theoretically be changed between validation and read. This is a standard filesystem race condition that affects all programs. Mitigation would require `O_NOFOLLOW` or similar, which would break legitimate symlink use cases.

**Recommendation**: If you don't trust symlinks in your project, remove them or use `--no-ignore` to skip gitignored paths where symlinks might hide.
```

### Replacement

```markdown
## Symlink Behavior

cqs has **two** symlink-handling regimes, depending on the entry point.

### Directory walks (`cqs index`, `cqs ref add`, `cqs watch` reconcile, `cqs convert`)

Symlinks are **skipped** entirely — `enumerate_files` (`src/lib.rs:601`) sets `WalkBuilder::follow_links(false)` and `cqs convert`'s archive extraction skips them in extract paths. The walker never opens the link's target.

| Scenario | Behavior |
|----------|----------|
| `project/link → project/src/file.rs` | Skipped (symlink, regardless of target) |
| `project/link → /etc/passwd` | Skipped |
| `project/link → ../sibling/file` | Skipped |

This is conservative: a monorepo workspace that uses in-tree symlinks to share common code will silently miss those files. Workaround: replace the symlinks with the actual files (or use a `[references]` config block to index the shared tree as a separate slot).

### Explicit-path canonicalization (`cqs read <path>`, `cqs ref add --source <path>`)

When the user passes a path on the command line, cqs canonicalizes it (`dunce::canonicalize`), then validates the resolved path against the project root.

| Scenario | Behavior |
|----------|----------|
| `cqs read link` where `link → project/src/file.rs` | Allowed (target inside project, canonicalised path reads `project/src/file.rs`) |
| `cqs read link` where `link → /etc/passwd` | Blocked (target outside project) |
| `cqs read link` where `link → ../sibling/file` | Blocked (target outside project) |

**TOCTOU consideration**: A symlink could theoretically be changed between canonicalization and read. This is a standard filesystem race condition that affects all programs. Mitigation would require `O_NOFOLLOW` or similar, which would break legitimate symlink use cases on `cqs read`.

**Recommendation**: If you don't trust symlinks in your project, remove them. The directory-walk path is already conservative.
```

### Verification

- `grep -n "follow_links" /mnt/c/Projects/cqs/src/lib.rs` — confirm `follow_links(false)` still in place.
- `grep -n "Symlink filtering" /mnt/c/Projects/cqs/SECURITY.md` — line 162 should still read "Symlinks are skipped during directory walks and archive extraction" (consistent with new matrix).
- No code build needed (doc-only).

---

## P1.6: SEC-V1.30.1-1 — SECURITY claims `read --focus` / `context` carry `trust_level` (they don't)

**Files:** `SECURITY.md:57`, `src/cli/commands/io/read.rs:310-323`, `src/cli/commands/io/context.rs:219-248`
**Effort:** ~30 minutes
**Why:** SECURITY.md:57 lists `read`, `read --focus`, and `context` as JSON outputs that carry `trust_level: "user-code" | "reference-code"` and per-chunk `injection_flags: []`. Verification:
- `FocusedReadJsonOutput` (read.rs:310-323) has fields `{focus, content, hints, warnings}` — no `trust_level`, no `injection_flags`.
- `FullChunkEntry` (context.rs:239-248) has `{name, chunk_type, signature, line_start, line_end, doc, content}` — no `trust_level`, no `injection_flags`.
- The `tag_user_code_trust_level` walker only walks scout/onboard shapes (`entry_point`, `call_chain[]`, `callers[]`, `file_groups[].chunks[]`).

Per "Docs Lying Is P1": both the doc claim *and* the code must be brought in line. Cheapest correct fix: extend the JSON shapes to carry the field (fixed value `"user-code"` for project-store paths; `"reference-code"` if the path resolves to a `cqs ref` index — but `read --focus` and `context` always read from the project store, so this fix is the constant `"user-code"` plus the empty `injection_flags`).

### Current code

```rust
//   src/cli/commands/io/read.rs:310-323
/// JSON output for a focused read.
#[derive(Debug, serde::Serialize)]
struct FocusedReadJsonOutput {
    focus: String,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    hints: Option<ReadHints>,
    /// P2.23: warnings emitted by the underlying assembly (e.g.
    /// `search_by_names_batch` failed). Mirrors `SummaryOutput::warnings`
    /// per EH-V1.29-9 — agents need to distinguish "no type deps" from
    /// "type-deps lookup failed silently".
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
}
```

```rust
//   src/cli/commands/io/context.rs:237-248
/// A chunk in full context output.
#[derive(Debug, serde::Serialize)]
pub(crate) struct FullChunkEntry {
    pub name: String,
    pub chunk_type: String,
    pub signature: String,
    pub line_start: u32,
    pub line_end: u32,
    pub doc: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}
```

### Replacement

```rust
//   src/cli/commands/io/read.rs:310-323
/// JSON output for a focused read.
#[derive(Debug, serde::Serialize)]
struct FocusedReadJsonOutput {
    focus: String,
    content: String,
    /// SEC-V1.30.1-1: every chunk-returning JSON output must carry a
    /// trust_level. `read --focus` reads from the project store only
    /// (no reference-store fan-in), so this is always "user-code".
    /// SECURITY.md's mitigation contract is that agents can branch
    /// safely on this field; the `read --focus` path was missing it.
    trust_level: &'static str,
    /// SEC-V1.30.1-1: parallel field to chunk JSON. `read --focus`
    /// content is delivered as a single concatenated string, not a
    /// per-chunk list, so there is no per-chunk array — a single
    /// empty array satisfies the schema-stability contract.
    injection_flags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hints: Option<ReadHints>,
    /// P2.23: warnings emitted by the underlying assembly (e.g.
    /// `search_by_names_batch` failed). Mirrors `SummaryOutput::warnings`
    /// per EH-V1.29-9 — agents need to distinguish "no type deps" from
    /// "type-deps lookup failed silently".
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
}
```

```rust
//   src/cli/commands/io/context.rs:237-248
/// A chunk in full context output.
#[derive(Debug, serde::Serialize)]
pub(crate) struct FullChunkEntry {
    pub name: String,
    pub chunk_type: String,
    pub signature: String,
    pub line_start: u32,
    pub line_end: u32,
    pub doc: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// SEC-V1.30.1-1: every chunk-returning JSON output must carry a
    /// trust_level. `cqs context` reads from the project store only;
    /// always "user-code". SECURITY.md mitigation contract.
    pub trust_level: &'static str,
    /// SEC-V1.30.1-1: per-chunk injection-heuristic flags. The full
    /// per-content-scan integration is #1181 follow-up; for now the
    /// schema-stability contract requires the field be present and an
    /// empty `Vec<String>` reflects "no heuristics fired".
    pub injection_flags: Vec<String>,
}
```

Then update the construction sites. In `read.rs`, the production constructor at line 408-413 becomes:

```rust
        let output = FocusedReadJsonOutput {
            focus: focus.to_string(),
            content: result.output,
            trust_level: "user-code",
            injection_flags: Vec::new(),
            hints,
            warnings: result.warnings.clone(),
        };
```

The `#[cfg(test)] mod tests` block in the same file has 3 additional `FocusedReadJsonOutput { ... }` literals that the new required fields will break unless updated. The exact sites are:

`src/cli/commands/io/read.rs:447` (`focused_read_output_with_hints`):
```rust
        let output = FocusedReadJsonOutput {
            focus: "search".into(),
            content: "fn search() { ... }".into(),
            trust_level: "user-code",
            injection_flags: Vec::new(),
            hints: Some(ReadHints {
                caller_count: 3,
                test_count: 2,
                no_callers: false,
                no_tests: false,
            }),
            warnings: Vec::new(),
        };
```

`src/cli/commands/io/read.rs:470` (`focused_read_output_no_hints`):
```rust
        let output = FocusedReadJsonOutput {
            focus: "MyStruct".into(),
            content: "struct MyStruct {}".into(),
            trust_level: "user-code",
            injection_flags: Vec::new(),
            hints: None,
            warnings: Vec::new(),
        };
```

`src/cli/commands/io/read.rs:486` (`focused_read_output_with_warnings`):
```rust
        let output = FocusedReadJsonOutput {
            focus: "MyStruct".into(),
            content: "struct MyStruct {}".into(),
            trust_level: "user-code",
            injection_flags: Vec::new(),
            hints: None,
            warnings: vec!["search_by_names_batch failed: db locked".into()],
        };
```

In `context.rs`, the `FullChunkEntry` constructor at line 281-289 becomes:

```rust
            FullChunkEntry {
                name: c.name.clone(),
                chunk_type: c.chunk_type.to_string(),
                signature: c.signature.clone(),
                line_start: c.line_start,
                line_end: c.line_end,
                doc: c.doc.clone(),
                content,
                trust_level: "user-code",
                injection_flags: Vec::new(),
            }
```

The `CompactChunkEntry` and `SummaryChunkEntry` structs also need the new fields, since SECURITY.md:57 names `read`, `read --focus`, *and* `context` as carriers of `trust_level`. `cqs context --compact` and `cqs context --summary` both fall under the `context` surface.

`src/cli/commands/io/context.rs:60-68` — extend the struct:
```rust
/// A single chunk in compact context output.
#[derive(Debug, serde::Serialize)]
pub(crate) struct CompactChunkEntry {
    pub name: String,
    pub chunk_type: String,
    pub signature: String,
    pub line_start: u32,
    pub line_end: u32,
    pub caller_count: u64,
    pub callee_count: u64,
    /// SEC-V1.30.1-1: every chunk-returning JSON output must carry a
    /// trust_level. `cqs context --compact` reads from the project store
    /// only; always "user-code".
    pub trust_level: &'static str,
    /// SEC-V1.30.1-1: per-chunk injection-heuristic flags. Empty for now;
    /// schema-stability contract requires the field be present.
    pub injection_flags: Vec<String>,
}
```

`src/cli/commands/io/context.rs:84` — update the constructor inside `compact_to_json`:
```rust
            CompactChunkEntry {
                name: c.name.clone(),
                chunk_type: c.chunk_type.to_string(),
                signature: c.signature.clone(),
                line_start: c.line_start,
                line_end: c.line_end,
                caller_count: cc,
                callee_count: ce,
                trust_level: "user-code",
                injection_flags: Vec::new(),
            }
```

`src/cli/commands/io/context.rs:472-477` — extend `SummaryChunkEntry`:
```rust
/// A chunk in summary context output.
#[derive(Debug, serde::Serialize)]
pub(crate) struct SummaryChunkEntry {
    pub name: String,
    pub chunk_type: String,
    pub line_start: u32,
    pub line_end: u32,
    /// SEC-V1.30.1-1: every chunk-returning JSON output must carry a
    /// trust_level. `cqs context --summary` reads from the project store
    /// only; always "user-code".
    pub trust_level: &'static str,
    /// SEC-V1.30.1-1: per-chunk injection-heuristic flags.
    pub injection_flags: Vec<String>,
}
```

`src/cli/commands/io/context.rs:486` — update the constructor inside `summary_to_json`:
```rust
        .map(|c| SummaryChunkEntry {
            name: c.name.clone(),
            chunk_type: c.chunk_type.to_string(),
            line_start: c.line_start,
            line_end: c.line_end,
            trust_level: "user-code",
            injection_flags: Vec::new(),
        })
```

If existing integration tests in `context.rs` (e.g. `hp1_compact_to_json_*`) construct any of these structs literally, they need the same two fields added.

### Verification

- `cargo build --features cuda-index`
- `cargo test --features cuda-index --lib io::read`
- `cargo test --features cuda-index --lib io::context`
- Manual: `cqs read --focus some_function --json | jq '.trust_level, .injection_flags'` should print `"user-code"` and `[]`.
- Manual: `cqs context some_file.rs --full --json | jq '.chunks[0].trust_level, .chunks[0].injection_flags'` should print the same.
- Manual: `cqs context some_file.rs --compact --json | jq '.chunks[0].trust_level'` and `cqs context some_file.rs --summary --json | jq '.chunks[0].trust_level'` should also print `"user-code"`.
- `grep -n "trust_level\|injection_flags" /mnt/c/Projects/cqs/SECURITY.md` — confirm SECURITY.md:57 and :61 still read accurately (no doc edit needed once code matches).

---

## P1.7: DOC-V1.30.1-7 — SECURITY.md auth claim missing cookie + `NoAuthAcknowledgement`

**Files:** `SECURITY.md:17`
**Effort:** ~5 minutes
**Why:** SECURITY.md:17 describes the auth surface as the v1.29 shape — "cookie handoff is `HttpOnly; SameSite=Strict`; compare is constant-time. `--no-auth` opts out". Two v1.30 hardenings landed and aren't reflected:
- (#1135) Cookie name renamed to `cqs_token_<port>` so concurrent `cqs serve` instances on the same host don't collide in the browser jar.
- (#1136) `--no-auth` now requires constructing a `NoAuthAcknowledgement` proof token; an internal caller cannot accidentally ship a fully-open server.

Per "Docs Lying Is P1": SECURITY.md is the canonical surface for auth claims; missing protections that were just landed makes the doc weaker than the code.

### Current docs

```markdown
| **`cqs serve` HTTP clients** | Untrusted by default | Per-launch 256-bit auth token gates every request (#1118 / SEC-7); cookie handoff is `HttpOnly; SameSite=Strict`; compare is constant-time. `--no-auth` opts out for scripted automation but is paired with a loud-warn banner on non-loopback binds. |
```

### Replacement

```markdown
| **`cqs serve` HTTP clients** | Untrusted by default | Per-launch 256-bit auth token gates every request (#1118 / SEC-7). Three credential channels: `Authorization: Bearer`, `cqs_token_<port>` cookie (port-scoped per RFC 6265, #1135 — concurrent instances don't collide in the browser jar), `?token=` query param. Cookie handoff is `HttpOnly; SameSite=Strict; Path=/`; compare is constant-time on every channel. Disabling auth requires `--no-auth` plus an internal `NoAuthAcknowledgement` proof token (#1136), so no internal caller can ship a fully-open server by accident; the disabled branch logs a structured `tracing::error!` regardless of `quiet`. Loud-warn banner on non-loopback binds with `--no-auth`. |
```

### Verification

- `grep -n "cqs_token_\|NoAuthAcknowledgement" /mnt/c/Projects/cqs/SECURITY.md` — should find at least one match (the new line above).
- `grep -rn "NoAuthAcknowledgement\|cookie_name_for_port" /mnt/c/Projects/cqs/src/serve/` — confirm both names exist in code (they do, per #1135/#1136 PRs).
- No code build needed (doc-only).

---

## P1.8: DOC-V1.30.1-4 — ROADMAP claims #1182 acceptance test pending; #1196 already merged

**Files:** `ROADMAP.md:16`, `ROADMAP.md:142`
**Effort:** ~3 minutes
**Why:** Both lines say "remaining acceptance item is the WSL-specific integration test" — but `git log` shows `a240ad08 test(watch): bulk-delta reconcile pass for #1182 acceptance — 47-file scenario (#1196)` already merged before v1.30.1 cut. The ROADMAP is now lying about open work; agents reading the roadmap will think there's still a test gap to fill and either duplicate work or distrust the rest of the doc.

### Current docs

ROADMAP.md:16:
```markdown
- [#1182](https://github.com/jamie8johnson/cqs/issues/1182) — **perfect watch mode (3-layer reconciliation).** Closes the missed-event classes (bulk git ops, WSL 9P, external writes) via `.git/hooks/post-{checkout,merge,rewrite}` + periodic full-tree fingerprint reconciliation + `cqs status --watch-fresh --wait` API. Promise: "the index is always either fresh or telling you it isn't." Supersedes the CLAUDE.md "always run `cqs index` after branch switches/merges" guidance. **Positioning lever:** *easy to index, hard to keep indexed between turns* — closing the gap promotes freshness to a top-line property alongside semantic search + call graphs. **Prior-art survey 2026-04-28** (in #1182 comment): codeindex.cc has per-query stale flags; Cursor has Merkle-tree sync; CocoIndex has fast incremental updates. None has the blocking `--wait` API + git-hook integration + "between turns" consumer-consistency-model framing. Honest pitch: "the only code search tool that lets your agent **wait** until it's fresh." Marketing claim: closing a known gap with a more complete design, not inventing a new category. **Status (2026-04-28):** Layers 1-4 shipped (#1189 freshness API, #1191 periodic reconciliation, #1193 git hooks, #1194 eval `--require-fresh`). Remaining: a WSL `/mnt/c/` integration test that exercises the full `git checkout` → freshness API stale → rebuild cycle.
```

ROADMAP.md:142:
```markdown
- [x] **#1182 — perfect watch mode (3-layer reconciliation).** Filed 2026-04-28. The closing-the-gap item. Three layers compose: (1) `.git/hooks/post-{checkout,merge,rewrite}` post a `reconcile` message to the daemon socket, (2) periodic full-tree fingerprint reconciliation every `CQS_WATCH_RECONCILE_SECS` (default 30s) catches what hooks + inotify miss, (3) `cqs status --watch-fresh --wait` exposes a freshness contract — eval-runner just calls `--wait` and stops caring. Promise: bounded eventual consistency, agent can either trust `fresh` or block. **Positioning differentiator. Layers 1-4 shipped #1189/#1191/#1193/#1194; remaining acceptance item is the WSL-specific integration test.**
```

### Replacement

ROADMAP.md:16 — replace the trailing **Status** sentence:
```markdown
**Status (2026-04-28):** Layers 1-4 shipped (#1189 freshness API, #1191 periodic reconciliation, #1193 git hooks, #1194 eval `--require-fresh`); 47-file bulk-delta acceptance test landed in #1196. #1182 fully closed.
```

ROADMAP.md:142 — replace the trailing **Positioning differentiator** sentence:
```markdown
**Positioning differentiator. Layers 1-4 shipped #1189/#1191/#1193/#1194; 47-file bulk-delta acceptance test landed in #1196.**
```

### Verification

- `grep -n "remaining acceptance item\|WSL-specific integration test" /mnt/c/Projects/cqs/ROADMAP.md` — should return zero matches.
- `git log --oneline --all | grep "#1196"` — confirms `a240ad08` is on main.
- No code build needed (doc-only).

---

## P1.9: SHL-V1.30-1 — `embed_batch_size_for` dead code; production OOMs nomic-coderank

**Files:** `src/cli/pipeline/types.rs:147-207`, `src/cli/pipeline/parsing.rs:14,42`, `src/cli/enrichment.rs:73-74`, `src/cli/pipeline/mod.rs:15`, `src/cli/commands/index/build.rs:520-522`
**Effort:** ~25 minutes
**Why:** P2.41 in v1.30.0 triage was marked "fixed (added `embed_batch_size_for(model)`; pipeline migration follow-on)" — but the helper is still `#[allow(dead_code)]` and zero production callers exist. Both `parser_stage` (parsing.rs:42) and `enrichment_pass` (enrichment.rs:74) call legacy `embed_batch_size()` which returns 64 regardless of model. `cqs index --model nomic-coderank` (768 dim, 2048 seq) at batch=64 ships with a known OOM config on RTX 4060 8GB. Same pattern as the configurable-models disaster from MEMORY.md.

### Current code

```rust
//   src/cli/pipeline/types.rs:178-179
#[allow(dead_code)] // P2.41: opt-in helper; pipeline migration is a follow-on PR.
pub(crate) fn embed_batch_size_for(model: &cqs::embedder::ModelConfig) -> usize {
```

```rust
//   src/cli/pipeline/parsing.rs:14
use super::types::{embed_batch_size, file_batch_size, ParsedBatch, RelationshipData};
```

```rust
//   src/cli/pipeline/parsing.rs:28-42 (signature + body)
pub(super) fn parser_stage(
    files: Vec<PathBuf>,
    ctx: ParserStageContext,
    parse_tx: Sender<ParsedBatch>,
) -> Result<()> {
    let _span = tracing::info_span!("parser_stage").entered();
    let ParserStageContext {
        root,
        force,
        parser,
        store,
        parsed_count,
        parse_errors,
    } = ctx;
    let batch_size = embed_batch_size();
    let file_batch_size = file_batch_size();
```

```rust
//   src/cli/enrichment.rs:23 (signature) and 73-74 (call site)
pub(crate) fn enrichment_pass(store: &Store, embedder: &Embedder, quiet: bool) -> Result<usize> {
    // ...
    // SHL-27: Use shared embed_batch_size() so CQS_EMBED_BATCH_SIZE env var is respected
    let enrich_embed_batch: usize = super::pipeline::embed_batch_size();
```

```rust
//   src/cli/pipeline/mod.rs:15
pub(crate) use types::embed_batch_size;
```

### Replacement

```rust
//   src/cli/pipeline/types.rs:178-179 (drop the dead-code suppression)
pub(crate) fn embed_batch_size_for(model: &cqs::embedder::ModelConfig) -> usize {
```

Also mark the legacy entry point with a clear test-only marker:

```rust
//   src/cli/pipeline/types.rs:139-164 (replace existing comment + body)
/// Legacy fixed-batch helper kept ONLY for callers without a `ModelConfig`
/// in scope (currently: nothing in production, only the in-tree tests
/// `pipeline::tests::test_embed_batch_size` and the parser-stage drain
/// regression test). Production must use [`embed_batch_size_for`] which
/// scales batch with the active model's dim & seq — at batch=64 the
/// nomic-coderank preset (768 dim, 2048 seq) OOMs an 8 GB GPU.
///
/// Returns 64 with `CQS_EMBED_BATCH_SIZE` env override.
#[cfg(test)]
pub(crate) fn embed_batch_size() -> usize {
    match std::env::var("CQS_EMBED_BATCH_SIZE") {
        Ok(val) => match val.parse::<usize>() {
            Ok(size) if size > 0 => {
                tracing::info!(batch_size = size, "CQS_EMBED_BATCH_SIZE override");
                size
            }
            _ => {
                tracing::warn!(
                    value = %val,
                    "Invalid CQS_EMBED_BATCH_SIZE, using default 64"
                );
                64
            }
        },
        Err(_) => 64,
    }
}
```

(The `#[cfg(test)]` gate is the structural guarantee — production grep for `embed_batch_size()` outside `#[cfg(test)]` blocks now fails to compile.)

Then update `pipeline/mod.rs:15`:

```rust
pub(crate) use types::embed_batch_size_for;
#[cfg(test)]
pub(crate) use types::embed_batch_size;
```

Update `parser_stage` to take a `ModelConfig` (already in scope at the only caller site, `pipeline/mod.rs:80-94`). Update its signature in `parsing.rs:18-25`:

```rust
//   src/cli/pipeline/parsing.rs:14 — adjust import
use super::types::{embed_batch_size_for, file_batch_size, ParsedBatch, RelationshipData};

//   src/cli/pipeline/parsing.rs:18-25 — extend the context struct
pub(super) struct ParserStageContext {
    pub root: PathBuf,
    pub force: bool,
    pub parser: Arc<CqParser>,
    pub store: Arc<Store>,
    pub parsed_count: Arc<AtomicUsize>,
    pub parse_errors: Arc<AtomicUsize>,
    pub model_config: cqs::embedder::ModelConfig,
}

//   src/cli/pipeline/parsing.rs:28-42 — read from ctx instead of hardcoded
pub(super) fn parser_stage(
    files: Vec<PathBuf>,
    ctx: ParserStageContext,
    parse_tx: Sender<ParsedBatch>,
) -> Result<()> {
    let _span = tracing::info_span!("parser_stage").entered();
    let ParserStageContext {
        root,
        force,
        parser,
        store,
        parsed_count,
        parse_errors,
        model_config,
    } = ctx;
    let batch_size = embed_batch_size_for(&model_config);
    let file_batch_size = file_batch_size();
```

In `pipeline/mod.rs:80-94` (the parser_handle spawn), thread `model_config`:

```rust
    let parser_handle = {
        let parser = Arc::clone(&parser);
        let store = Arc::clone(&store);
        let parsed_count = Arc::clone(&parsed_count);
        let parse_errors = Arc::clone(&parse_errors);
        let root = root.to_path_buf();
        let model_config = model_config.clone();
        thread::spawn(move || {
            parser_stage(
                files,
                ParserStageContext {
                    root,
                    force,
                    parser,
                    store,
                    parsed_count,
                    parse_errors,
                    model_config,
                },
                parse_tx,
            )
        })
    };
```

For `enrichment.rs`, plumb the `ModelConfig` through. Update the signature at line 23:

```rust
//   src/cli/enrichment.rs:23
pub(crate) fn enrichment_pass(
    store: &Store,
    embedder: &Embedder,
    model_config: &cqs::embedder::ModelConfig,
    quiet: bool,
) -> Result<usize> {
    let _span = tracing::info_span!("enrichment_pass").entered();
```

And at line 73-74 in `enrichment.rs`:

```rust
    // SHL-V1.30-1: model-aware batch size so nomic-coderank (768 dim,
    // 2048 seq) doesn't OOM at batch=64 on an 8 GB GPU.
    let enrich_embed_batch: usize = super::pipeline::embed_batch_size_for(model_config);
```

Update the only production caller at `src/cli/commands/index/build.rs:520-522`:

```rust
        let model_config = cli.try_model_config()?.clone();
        let embedder = Embedder::new(model_config.clone())
            .context("Failed to create embedder for enrichment pass")?;
        match enrichment_pass(&store, &embedder, &model_config, cli.quiet) {
```

Update test fixtures in `src/cli/pipeline/mod.rs` and `src/cli/pipeline/parsing.rs` that construct `ParserStageContext` directly to pass `model_config: cqs::embedder::ModelConfig::resolve(None, None)`. (`resolve` returns `Self` directly per `src/embedder/models.rs:427`, *not* a `Result`/`Option` — a stray `.unwrap()` here will fail to compile.) The pre-existing test `test_embed_batch_size` already serializes via `TEST_ENV_MUTEX`, so it stays valid using the test-only `embed_batch_size()`.

### Verification

- `cargo build --features cuda-index` (the `#[cfg(test)]` gate forces a compile-error on any production caller that still uses bare `embed_batch_size()`)
- `cargo build --features cuda-index 2>&1 | grep -i warning` — confirm no dead-code warnings on `embed_batch_size_for`
- `cargo test --features cuda-index --lib pipeline::tests::test_embed_batch_size`
- `cargo test --features cuda-index --lib pipeline::parsing::tests`
- Manual: `RUST_LOG=cqs::cli::pipeline=debug cqs index --model nomic-coderank` should log `embed_batch_size_for: model-derived default rounded=16` (768 dim, 2048 seq) instead of the legacy 64.
- `grep -rn "embed_batch_size()" /mnt/c/Projects/cqs/src/cli/ --include='*.rs' | grep -v cfg(test)` — should return zero non-test matches.

---

## P1.10: SEC-V1.30.1-8 — daemon env snapshot logs `CQS_LLM_API_KEY` to journal

**Files:** `src/cli/watch/mod.rs:525-532`
**Effort:** ~10 minutes
**Why:** On daemon startup, the code iterates every `CQS_*` env var and logs the values via `tracing::info!(cqs_vars = ?cqs_vars, "Daemon env snapshot")`. The list is not redacted. `CQS_LLM_API_KEY` (used by `src/llm/local.rs:110` per the audit) is one of the env knobs that flows through this snapshot if set. With OB-V1.30-1 having raised the default subscriber to surface info-level events to systemd-journald, every daemon start now writes the API key into a 30-day journal artifact. Same class as P1.13 (auth token printed to stdout).

### Current code

```rust
//   src/cli/watch/mod.rs:525-532
        // OB-NEW-2: Self-maintaining env snapshot — iterate every CQS_*
        // variable instead of a hardcoded whitelist that drifts as new
        // knobs are added. Env vars set on client subprocesses do NOT
        // affect daemon-served queries; only the daemon's own env applies.
        let cqs_vars: Vec<(String, String)> = std::env::vars()
            .filter(|(k, _)| k.starts_with("CQS_"))
            .collect();
        tracing::info!(cqs_vars = ?cqs_vars, "Daemon env snapshot");
```

### Replacement

```rust
        // OB-NEW-2 / SEC-V1.30.1-8: Self-maintaining env snapshot —
        // iterate every CQS_* variable instead of a hardcoded whitelist
        // that drifts as new knobs are added. Env vars set on client
        // subprocesses do NOT affect daemon-served queries; only the
        // daemon's own env applies.
        //
        // Redact secrets — any var whose name suffix matches a known
        // secret marker is logged with `<redacted len=N>` instead of
        // the value. With OB-V1.30-1 surfacing info-level to journald,
        // an unredacted log lands in a 30-day journal artifact.
        const SECRET_SUFFIXES: &[&str] =
            &["_API_KEY", "_TOKEN", "_PASSWORD", "_SECRET"];
        let cqs_vars: Vec<(String, String)> = std::env::vars()
            .filter(|(k, _)| k.starts_with("CQS_"))
            .map(|(k, v)| {
                let is_secret = SECRET_SUFFIXES
                    .iter()
                    .any(|suffix| k.ends_with(suffix));
                let value = if is_secret {
                    format!("<redacted len={}>", v.len())
                } else {
                    v
                };
                (k, value)
            })
            .collect();
        tracing::info!(cqs_vars = ?cqs_vars, "Daemon env snapshot");
```

### Verification

- `cargo build --features cuda-index`
- Add a regression test (in `src/cli/watch/tests.rs` or a new `#[cfg(test)] mod` in `mod.rs`):
  ```rust
  #[test]
  fn env_snapshot_redacts_api_key() {
      // CQS_LLM_API_KEY mustn't land in journald.
      // Build the same redaction logic and assert against a fixture.
      const SECRET_SUFFIXES: &[&str] =
          &["_API_KEY", "_TOKEN", "_PASSWORD", "_SECRET"];
      let pairs = vec![
          ("CQS_LLM_API_KEY".to_string(), "sk-real-secret".to_string()),
          ("CQS_TELEMETRY".to_string(), "1".to_string()),
      ];
      let redacted: Vec<(String, String)> = pairs
          .into_iter()
          .map(|(k, v)| {
              let is_secret = SECRET_SUFFIXES
                  .iter()
                  .any(|suffix| k.ends_with(suffix));
              let value = if is_secret {
                  format!("<redacted len={}>", v.len())
              } else {
                  v
              };
              (k, value)
          })
          .collect();
      assert_eq!(
          redacted[0].1, "<redacted len=14>",
          "CQS_LLM_API_KEY value must not appear in plaintext"
      );
      assert_eq!(redacted[1].1, "1", "CQS_TELEMETRY is non-secret, kept verbatim");
  }
  ```
- `cargo test --features cuda-index --lib env_snapshot_redacts_api_key`
- Manual: `CQS_LLM_API_KEY=sk-test-secret CQS_TELEMETRY=1 cqs watch --serve` then `journalctl --user-unit cqs-watch | grep cqs_vars` should show `<redacted len=14>`, never `sk-test-secret`.

---

## P1.11: DS-V1.30.1-D2 — `run_daemon_reconcile` bypasses `max_pending_files()` cap

**Files:** `src/cli/watch/reconcile.rs:63-148`, callers at `src/cli/watch/mod.rs:1055-1061,1268-1274`
**Effort:** ~20 minutes
**Why:** The inotify ingest path at `events.rs:108` enforces `pending_files.len() < max_pending_files()` before inserting and increments `dropped_this_cycle` when the queue is full. `run_daemon_reconcile` blindly inserts every divergent file with no cap check. On a `git checkout` of a sibling branch with 50k file changes, reconcile pushes the queue size to 50k. Two consequences: (1) the queue overshoots `max_pending_files()` (default 10000), making subsequent inotify drops look like sustained pressure when they're actually held above the cap by reconcile; (2) the next `process_file_changes` parses + embeds 50k synchronously while holding the index lock. The whole point of `max_pending_files()` is to bound per-cycle work; reconcile defeats it.

### Current code

```rust
//   src/cli/watch/reconcile.rs:63-148
pub(super) fn run_daemon_reconcile(
    store: &Store,
    root: &Path,
    parser: &CqParser,
    no_ignore: bool,
    pending_files: &mut HashSet<PathBuf>,
) -> usize {
    let _span = tracing::info_span!("daemon_reconcile").entered();

    // Walk disk → set of relative paths visible to indexing.
    let exts = parser.supported_extensions();
    let disk_files = match cqs::enumerate_files(root, &exts, no_ignore) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "Reconcile: enumerate_files failed");
            return 0;
        }
    };

    // One SELECT pulls every indexed source-file origin + its stored
    // mtime. Map keyed by origin string for cheap lookups in the loop.
    let indexed = match store.indexed_file_origins() {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(error = %e, "Reconcile: indexed_file_origins failed");
            return 0;
        }
    };

    let mut added = 0usize;
    let mut modified = 0usize;
    let mut queued = 0usize;
    for rel in disk_files {
        // Stored origins are typically relative; normalize to forward
        // slashes for cross-platform matching parity with the rest of the
        // store layer.
        let origin = rel.to_string_lossy().replace('\\', "/");
        match indexed.get(&origin) {
            None => {
                // ADDED: no chunks for this file in the index. Queue.
                if pending_files.insert(rel.clone()) {
                    added += 1;
                    queued += 1;
                }
            }
            Some(stored_mtime) => {
                // MODIFIED: same path indexed, but mtime moved forward.
                // `None` stored mtime → treat as stale (legacy schema).
                let lookup_path: PathBuf = if rel.is_absolute() {
                    rel.clone()
                } else {
                    root.join(&rel)
                };
                let disk_mtime = match lookup_path.metadata().and_then(|m| m.modified()) {
                    Ok(t) => t
                        .duration_since(std::time::UNIX_EPOCH)
                        .ok()
                        .map(cqs::duration_to_mtime_millis),
                    Err(_) => None,
                };
                let needs_reindex = match (stored_mtime, disk_mtime) {
                    (Some(stored), Some(disk)) => disk > *stored,
                    (None, _) => true,        // legacy/null stored mtime
                    (Some(_), None) => false, // can't read disk mtime → leave to GC
                };
                if needs_reindex && pending_files.insert(rel.clone()) {
                    modified += 1;
                    queued += 1;
                }
            }
        }
    }

    if queued > 0 {
        tracing::info!(
            queued,
            added,
            modified,
            "Reconcile: queued divergent files for reindex"
        );
    } else {
        tracing::debug!("Reconcile: no divergence detected");
    }

    queued
}
```

### Replacement

```rust
pub(super) fn run_daemon_reconcile(
    store: &Store,
    root: &Path,
    parser: &CqParser,
    no_ignore: bool,
    pending_files: &mut HashSet<PathBuf>,
    max_pending: usize,
) -> usize {
    let _span = tracing::info_span!("daemon_reconcile", max_pending).entered();

    // Walk disk → set of relative paths visible to indexing.
    let exts = parser.supported_extensions();
    let disk_files = match cqs::enumerate_files(root, &exts, no_ignore) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "Reconcile: enumerate_files failed");
            return 0;
        }
    };

    // One SELECT pulls every indexed source-file origin + its stored
    // mtime. Map keyed by origin string for cheap lookups in the loop.
    let indexed = match store.indexed_file_origins() {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(error = %e, "Reconcile: indexed_file_origins failed");
            return 0;
        }
    };

    let mut added = 0usize;
    let mut modified = 0usize;
    let mut queued = 0usize;
    let mut skipped_at_cap = 0usize;
    for rel in disk_files {
        // DS-V1.30.1-D2: respect the same cap as the inotify path so a
        // bulk branch switch (50k files) doesn't drown the next
        // `process_file_changes` cycle. Files we skip here are picked
        // up by the next reconcile pass — the walk is idempotent.
        if pending_files.len() >= max_pending {
            skipped_at_cap += 1;
            continue;
        }
        // Stored origins are typically relative; normalize to forward
        // slashes for cross-platform matching parity with the rest of the
        // store layer.
        let origin = rel.to_string_lossy().replace('\\', "/");
        match indexed.get(&origin) {
            None => {
                // ADDED: no chunks for this file in the index. Queue.
                if pending_files.insert(rel.clone()) {
                    added += 1;
                    queued += 1;
                }
            }
            Some(stored_mtime) => {
                // MODIFIED: same path indexed, but mtime moved forward.
                // `None` stored mtime → treat as stale (legacy schema).
                let lookup_path: PathBuf = if rel.is_absolute() {
                    rel.clone()
                } else {
                    root.join(&rel)
                };
                let disk_mtime = match lookup_path.metadata().and_then(|m| m.modified()) {
                    Ok(t) => t
                        .duration_since(std::time::UNIX_EPOCH)
                        .ok()
                        .map(cqs::duration_to_mtime_millis),
                    Err(_) => None,
                };
                let needs_reindex = match (stored_mtime, disk_mtime) {
                    (Some(stored), Some(disk)) => disk > *stored,
                    (None, _) => true,        // legacy/null stored mtime
                    (Some(_), None) => false, // can't read disk mtime → leave to GC
                };
                if needs_reindex && pending_files.insert(rel.clone()) {
                    modified += 1;
                    queued += 1;
                }
            }
        }
    }

    if skipped_at_cap > 0 {
        tracing::warn!(
            queued,
            skipped_at_cap,
            cap = max_pending,
            "Reconcile: hit pending-files cap; skipped files will be picked up on next reconcile pass"
        );
    } else if queued > 0 {
        tracing::info!(
            queued,
            added,
            modified,
            "Reconcile: queued divergent files for reindex"
        );
    } else {
        tracing::debug!("Reconcile: no divergence detected");
    }

    queued
}
```

Update the two production callers in `src/cli/watch/mod.rs`:

```rust
//   src/cli/watch/mod.rs:1055-1061 (on-demand reconcile)
                if on_demand_reconcile_requested && reconcile_enabled_flag {
                    let queued = run_daemon_reconcile(
                        &store,
                        &root,
                        &parser,
                        no_ignore,
                        &mut state.pending_files,
                        max_pending_files(),
                    );
```

```rust
//   src/cli/watch/mod.rs:1268-1274 (periodic reconcile)
                        let queued = run_daemon_reconcile(
                            &store,
                            &root,
                            &parser,
                            no_ignore,
                            &mut state.pending_files,
                            max_pending_files(),
                        );
```

Update the 5 test call sites in `src/cli/watch/reconcile.rs:190,204,225,362,450` to pass a sentinel cap (e.g. `usize::MAX` for unbounded test behaviour, or a small number for cap-respect tests). Existing tests should mostly use `usize::MAX` so behaviour is unchanged; add a new test that exercises the cap:

```rust
    #[test]
    fn run_daemon_reconcile_respects_max_pending_cap() {
        // DS-V1.30.1-D2: cap shared with the inotify path so a bulk
        // git-checkout doesn't drown the next process_file_changes
        // cycle.
        let dir = TempDir::new().unwrap();
        let cqs_dir = dir.path().join(".cqs");
        fs::create_dir_all(&cqs_dir).unwrap();
        // Use the existing `open_store` helper at reconcile.rs:170 — it
        // takes the `.cqs/` dir, not the project root. There is no
        // `setup_store_for_test`; pinning that name was an artifact of
        // a draft.
        let store = open_store(&cqs_dir);

        let mut pending: HashSet<PathBuf> = HashSet::new();
        // Pre-fill 5 entries so `pending.len() >= cap=5` immediately.
        for i in 0..5 {
            pending.insert(PathBuf::from(format!("preexisting_{i}.rs")));
        }
        // Create 20 files on disk.
        let src_dir = dir.path().join("src");
        fs::create_dir_all(&src_dir).unwrap();
        for i in 0..20 {
            fs::write(src_dir.join(format!("file_{i}.rs")), "fn x(){}").unwrap();
        }
        let queued = run_daemon_reconcile(
            &store,
            dir.path(),
            &parser(),
            false,
            &mut pending,
            5, // cap is already met
        );
        assert_eq!(queued, 0, "cap already met → no new entries queued");
        assert_eq!(pending.len(), 5, "pending must not exceed cap");
    }
```

### Verification

- `cargo build --features cuda-index`
- `cargo test --features cuda-index --lib watch::reconcile`
- `cargo test --features cuda-index --lib watch::reconcile::tests::run_daemon_reconcile_respects_max_pending_cap`
- Manual: with `CQS_WATCH_MAX_PENDING=10`, `git checkout` of a 47-file diff should leave `pending_files.len() == 10`, with the journal showing `Reconcile: hit pending-files cap; skipped files will be picked up on next reconcile pass cap=10 skipped_at_cap=37`.

---

## P1.12: AC-V1.30.1-1 — reconcile `disk > stored` strict predicate misses non-monotonic checkouts

**Files:** `src/cli/watch/reconcile.rs:108-127`
**Effort:** ~25 minutes
**Why:** The reconcile predicate is `(stored, disk) => disk > *stored`. Reconcile is the Layer 2 safety net for bulk git operations the inotify path misses. But `git checkout` of a sibling branch restores file mtimes to **commit time** — easily *older* than the indexed `source_mtime`. Concrete repro: index `foo.rs` at HEAD (mtime=now), then `git checkout HEAD~5 -- foo.rs` where `HEAD~5` is from last week. Disk content is now different, but `disk_mtime <= stored_mtime`, so reconcile classifies the file as "fine" and skips it. The inotify path also uses `mtime <= last` (events.rs:100) — silently stale until either `cqs index --force` or the file is touched again. The bulk-delta acceptance test (#1196) only exercises forward-mtime cases.

### Current code

```rust
//   src/cli/watch/reconcile.rs:108-132
            Some(stored_mtime) => {
                // MODIFIED: same path indexed, but mtime moved forward.
                // `None` stored mtime → treat as stale (legacy schema).
                let lookup_path: PathBuf = if rel.is_absolute() {
                    rel.clone()
                } else {
                    root.join(&rel)
                };
                let disk_mtime = match lookup_path.metadata().and_then(|m| m.modified()) {
                    Ok(t) => t
                        .duration_since(std::time::UNIX_EPOCH)
                        .ok()
                        .map(cqs::duration_to_mtime_millis),
                    Err(_) => None,
                };
                let needs_reindex = match (stored_mtime, disk_mtime) {
                    (Some(stored), Some(disk)) => disk > *stored,
                    (None, _) => true,        // legacy/null stored mtime
                    (Some(_), None) => false, // can't read disk mtime → leave to GC
                };
                if needs_reindex && pending_files.insert(rel.clone()) {
                    modified += 1;
                    queued += 1;
                }
            }
```

### Replacement

```rust
            Some(stored_mtime) => {
                // MODIFIED: same path indexed, but disk content may have
                // diverged. `None` stored mtime → treat as stale (legacy
                // schema).
                let lookup_path: PathBuf = if rel.is_absolute() {
                    rel.clone()
                } else {
                    root.join(&rel)
                };
                let disk_mtime = match lookup_path.metadata().and_then(|m| m.modified()) {
                    Ok(t) => t
                        .duration_since(std::time::UNIX_EPOCH)
                        .ok()
                        .map(cqs::duration_to_mtime_millis),
                    Err(_) => None,
                };
                // AC-V1.30.1-1: use `!=` not `>` because `git checkout`
                // restores commit-time mtimes, which can be *older* than
                // the indexed `source_mtime`. The inotify path's
                // `mtime <= last` mtime-equality skip is correct for
                // single-file edits (where mtime always advances), but
                // reconcile exists *specifically* for bulk git ops where
                // mtime is non-monotonic. Any disk/stored mismatch is
                // a queue trigger; the reindex itself is content-hashed
                // so a no-op rewrite costs only the parse + cache-hit.
                let needs_reindex = match (stored_mtime, disk_mtime) {
                    (Some(stored), Some(disk)) => disk != *stored,
                    (None, _) => true,        // legacy/null stored mtime
                    (Some(_), None) => false, // can't read disk mtime → leave to GC
                };
                if needs_reindex && pending_files.insert(rel.clone()) {
                    modified += 1;
                    queued += 1;
                }
            }
```

Add a regression test alongside the existing reconcile tests in the same file's `#[cfg(test)]` block. The test uses two helpers already in scope: `open_store` at `reconcile.rs:170` and `placeholder_embedding` at `reconcile.rs:271`. Disk-mtime rewinds use `std::fs::File::set_modified` (stable since Rust 1.75 — same pattern already in use at `src/store/migrations.rs:2635` and `src/cli/batch/mod.rs:2763`), which avoids adding `filetime` as a new dev-dependency:

```rust
    /// AC-V1.30.1-1: `git checkout HEAD~5 -- foo.rs` restores the file
    /// with its commit-time mtime, which is *older* than the indexed
    /// `source_mtime`. The strict `disk > stored` predicate would skip
    /// this file silently. Reconcile must use `disk != stored` so any
    /// divergence — forward or backward in time — queues a reindex.
    #[test]
    fn run_daemon_reconcile_queues_older_disk_mtime() {
        use cqs::parser::{Chunk, ChunkType, Language};
        use std::time::{Duration, SystemTime};

        let dir = TempDir::new().unwrap();
        let cqs_dir = dir.path().join(".cqs");
        fs::create_dir_all(&cqs_dir).unwrap();
        let src_dir = dir.path().join("src");
        fs::create_dir_all(&src_dir).unwrap();

        // Write the file with new content (post-checkout state).
        let rel = "src/foo.rs";
        let abs = dir.path().join(rel);
        fs::write(&abs, "fn rewound() {}").unwrap();

        // Rewind the disk mtime to a week ago to simulate `git checkout`
        // restoring a commit-time mtime older than what we'll seed as
        // the stored mtime. `set_modified` is stable since Rust 1.75
        // (cqs MSRV is 1.95).
        let week_ago = SystemTime::now() - Duration::from_secs(7 * 24 * 60 * 60);
        let f = std::fs::OpenOptions::new().write(true).open(&abs).unwrap();
        f.set_modified(week_ago).unwrap();
        drop(f);

        // Seed the index with a HIGHER stored_mtime than the rewound
        // disk mtime — simulates "indexed at HEAD (today), then file
        // rewound by checkout to last week's commit". Use a "now" stored
        // mtime in milliseconds; even if the test runs millis after the
        // rewind, `now > week_ago` by a comfortable margin.
        let stored_mtime_ms = cqs::duration_to_mtime_millis(
            SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap(),
        );
        let content = "fn original() {}".to_string(); // any content; only mtime drives the predicate
        let hash = blake3::hash(content.as_bytes()).to_hex().to_string();
        let chunk = Chunk {
            id: format!("{rel}:1:{}", &hash[..8]),
            file: PathBuf::from(rel),
            language: Language::Rust,
            chunk_type: ChunkType::Function,
            name: "original".to_string(),
            signature: "fn original()".to_string(),
            content,
            doc: None,
            line_start: 1,
            line_end: 1,
            content_hash: hash,
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        };

        let store = open_store(&cqs_dir);
        store
            .upsert_chunks_batch(
                &[(chunk, placeholder_embedding(0.0))],
                Some(stored_mtime_ms),
            )
            .expect("seed chunk at stored mtime");

        let mut pending: HashSet<PathBuf> = HashSet::new();
        let queued = run_daemon_reconcile(
            &store,
            dir.path(),
            &parser(),
            false,
            &mut pending,
            usize::MAX,
        );

        assert_eq!(queued, 1, "older-mtime divergent file must be queued");
        assert!(pending.contains(&PathBuf::from(rel)));
    }
```

(No `filetime` dependency: `std::fs::File::set_modified` is stable since Rust 1.75 and cqs MSRV is 1.95. The `open_store` and `placeholder_embedding` helpers already in `mod tests` are reused. Existing test pattern at `reconcile_detects_bulk_modify_burst` (line 304) shows the `upsert_chunks_batch` + explicit `stored_mtime_ms` shape this test mirrors.)

### Verification

- `cargo build --features cuda-index`
- `cargo test --features cuda-index --lib watch::reconcile::tests::run_daemon_reconcile_queues_older_disk_mtime`
- `cargo test --features cuda-index --lib watch::reconcile`
- Manual on a real repo: `git checkout HEAD~5 -- src/lib.rs` then wait `CQS_WATCH_RECONCILE_SECS` seconds. `cqs status --watch-fresh --json` should show `state: "stale"` until reconcile drains; before the fix it would stay `fresh` because `disk_mtime < stored_mtime` skipped the file.

---

## Summary

**12 distinct fix prompts cover the 14 P1 findings.** Three groupings collapsed multiple cross-listings: P1.1 (3 IDs → 1 prompt for `delta_saturated`), P1.2 (3 IDs → 1 prompt for `dropped_this_cycle`), P1.3 (2 IDs → 1 prompt for the auth ladder rewrite).

**Top 5 most-touched files:**
1. `src/watch_status.rs` — P1.1 state machine + tests
2. `src/cli/watch/events.rs` — P1.2 reset ordering
3. `src/serve/auth.rs` — P1.3 strip + check_request rewrite
4. `src/cli/watch/reconcile.rs` — P1.11 cap + P1.12 mtime predicate
5. `src/cli/pipeline/types.rs` + `parsing.rs` + `enrichment.rs` + `mod.rs` (all four touched by P1.9)

**Documentation files:** SECURITY.md (P1.5, P1.7), PRIVACY.md (P1.4), ROADMAP.md (P1.8), and one fix that requires both code-change + doc-claim alignment (P1.6 — adding `trust_level` to `read --focus` and `context` so SECURITY.md:57 is no longer lying).

**No P1 was skipped.** All 14 finding IDs in the triage's P1 table are covered by the 12 prompts above.

---

## Verification Report

Generated 2026-04-28 by the verification pass. Each P1 prompt was checked against current source for line-drift, compile-correctness, edge-case coverage, caller-sweep completeness, and lying-doc fix completeness.

### P1.1 — delta_saturated: VERIFIED

All current/replacement code matches source verbatim (`src/watch_status.rs:199-209` for `compute()`, fields on `WatchSnapshotInput` at lines 181-193, struct field on `WatchSnapshot` at line 87). Test field types align (`incremental_count: usize`, `dropped_this_cycle: usize`, `last_event: std::time::Instant`). New tests cover the failure mode: `delta_saturated_marks_stale_when_no_other_work` triggers the saturated-rebuild → discard-on-swap path the audit finding describes. Caller sweep clean (no new call sites required since `compute()` signature is unchanged).

### P1.2 — dropped_this_cycle reset before publish: VERIFIED

Current code at `src/cli/watch/events.rs:131-157` matches verbatim. The successful-reindex `Ok(...)` arm starts at line 195 (prompt cite "around line 195" — exact). Replacement removes the early reset and adds it inside the success arm — the exact ordering fix the audit identifies. The verification regression-test design (seed `dropped_this_cycle=5`, fail embedder, assert no zero) matches the failure mode.

### P1.3 — auth ladder leaks ?token=: VERIFIED

Current code at `src/serve/auth.rs:243-321` (both `strip_token_param` and `check_request`) matches verbatim. The two pinned tests at lines 572-600 exist with the exact `_NOT_stripped_today` / `_today_rejects` naming. `axum::body::Body` is in scope at the module level (line 35), so `Body::empty()` in the new test compiles. `AuthToken::random()` and `cookie_name_for_port` exist as cited. The `pair_key_is_token` helper handles all the case-fold + percent-decode cases the audit names. The `!pair.is_empty()` filter cleanup is consistent with the existing `p2_30_strip_token_param_handles_double_ampersand` test's permissive assertion (`==` either form). `percent-encoding` is in the lockfile transitively but not a direct dep — prompt correctly calls out the explicit add to `Cargo.toml`.

### P1.4 — PRIVACY/SECURITY misstate cache key: NEEDS FIX

- **Issue:** PRIVACY.md replacement claims "the `purpose` discriminator (`embedding` vs `summary`)" — but `src/cache.rs:99-104` shows `CachePurpose` is `Embedding ("embedding")` and `EmbeddingBase ("embedding_base")`. There is no `'summary'` purpose in the `embedding_cache` table; the `'summary'` purpose lives in a different table (`llm_summaries` per `src/schema.sql:182`).
- **Issue:** SECURITY.md replacement says "the post-summary embedding is cached in `embeddings_cache.db` keyed by `(content_hash, model_fingerprint, purpose='summary')` (#1128)" — same conflation. The post-summary embedding actually goes through the `Embedding` purpose path (no separate `summary` purpose row in `embedding_cache`).
- **Correction (PRIVACY.md:16):**
  ```markdown
  - `.cqs/embeddings_cache.db` — per-project embedding cache, keyed by `(content_hash, model_fingerprint, purpose)` (#1105, #1128). Skips re-embedding chunks that haven't changed across reindexes / model swaps; the `purpose` discriminator (`embedding` for the post-enrichment vector, `embedding_base` for the raw NL vector) prevents the two streams from overwriting each other when the same chunk produces both.
  ```
- **Correction (SECURITY.md:47):**
  ```markdown
  | **LLM-generated summaries** (`cqs index --llm-summaries`) | Claude is prompted with chunk content; a poisoned chunk can produce a summary that contains injection text. The summary is cached in `llm_summaries` keyed by `(content_hash, purpose)` per `src/schema.sql:178-182`; the post-summary embedding flows through the normal `embeddings_cache.db` (purpose `embedding`) and is replayed to downstream agents | Yes — cached in `llm_summaries` table + `embeddings_cache.db` |
  ```

### P1.5 — Symlink Behavior matrix: VERIFIED

Current docs at `SECURITY.md:203-215` match verbatim. `enumerate_files` at `src/lib.rs:601` uses `WalkBuilder::follow_links(false)` as cited. The split-into-two-regimes replacement is accurate to actual code paths. Cross-check with `SECURITY.md:162` ("Symlinks are skipped during directory walks and archive extraction") confirmed consistent.

### P1.6 — read --focus / context trust_level: NEEDS FIX

- **Issue:** Prompt says "find the `FocusedReadJsonOutput { ... }` literal (around line 369-380)" — actual production literal is at `src/cli/commands/io/read.rs:408`. More importantly, there are 3 additional `FocusedReadJsonOutput { ... }` literals inside `#[cfg(test)] mod tests` at lines 447, 470, 486 (`focused_read_output_with_hints`, `focused_read_output_no_hints`, `focused_read_output_with_warnings`). Adding required `trust_level` and `injection_flags` fields to the struct without updating these 3 test sites breaks the build.
- **Issue:** For `context.rs`, the prompt says "Apply the same pattern to `compact_to_json` and `summary_to_json` constructors" but does not show the actual constructor sites. `CompactChunkEntry` literal is at `src/cli/commands/io/context.rs:84`, `SummaryChunkEntry` literal at line 486. Both these structs *also* lack the new fields, so the audit-finding's claim that the JSON shape is consistent across all chunk-emitting commands implies these structs need the same `trust_level` + `injection_flags` additions as `FullChunkEntry`. The prompt omits the explicit struct-definition edits.
- **Correction:** Add explicit edits to (1) the 3 test-site `FocusedReadJsonOutput` constructions, (2) the `CompactChunkEntry` struct definition + `compact_to_json` constructor at line 84, (3) the `SummaryChunkEntry` struct definition + `summary_to_json` constructor at line 486. Existing integration tests in the same file (`hp1_compact_to_json_*` etc.) will also need their literal constructions updated.

### P1.7 — SECURITY auth surface backfill: VERIFIED

Current SECURITY.md:17 matches verbatim. `NoAuthAcknowledgement` exported at `src/serve/mod.rs:47`, `cookie_name_for_port` at `src/serve/auth.rs:62`. The replacement accurately reflects the v1.30 hardenings (#1135 cookie scoping + #1136 ack token).

### P1.8 — ROADMAP #1182 closed: VERIFIED

ROADMAP.md:16 and :142 match verbatim. Recent git log confirms `a240ad08 test(watch): bulk-delta reconcile pass for #1182 acceptance — 47-file scenario (#1196)` is on main. The replacement accurately closes the doc claim.

### P1.9 — embed_batch_size_for wiring: NEEDS FIX

- **Issue:** Prompt's test fixture update says "pass `model_config: cqs::embedder::ModelConfig::resolve(None, None).unwrap()`". But `ModelConfig::resolve` at `src/embedder/models.rs:427` returns `Self`, not `Result<Self>` or `Option<Self>`. `.unwrap()` on a non-`Result`/`Option` will not compile.
- **Correction:** Use `cqs::embedder::ModelConfig::resolve(None, None)` (no `.unwrap()`).
- All other line citations verified (types.rs:147-207, parsing.rs:14-42, enrichment.rs:23 + 73-74, pipeline/mod.rs:15 + 74-94, build.rs:520-522). Test fixture site at `parsing.rs:344` does construct `ParserStageContext` directly and would need the `model_config` field added — prompt mentions this generically.

### P1.10 — daemon env snapshot redaction: VERIFIED

Current code at `src/cli/watch/mod.rs:525-532` matches verbatim. `CQS_LLM_API_KEY` is referenced at `src/llm/local.rs:110` as cited. The redaction list `["_API_KEY", "_TOKEN", "_PASSWORD", "_SECRET"]` covers the named threat. Test design captures the exact in-prod logic.

### P1.11 — run_daemon_reconcile cap: NEEDS FIX

- **Issue:** Prompt's regression test calls `setup_store_for_test(dir.path())` — but no such helper exists in `src/cli/watch/reconcile.rs`. The actual helper is `open_store(cqs_dir)` at line 170, which takes the `.cqs/` dir, not the project root.
- **Correction:** Replace the test's setup with the existing helper pattern:
  ```rust
  let dir = tempfile::tempdir().unwrap();
  let cqs_dir = dir.path().join(".cqs");
  std::fs::create_dir_all(&cqs_dir).unwrap();
  let store = open_store(&cqs_dir);
  ```
- All other citations verified: `run_daemon_reconcile` at lines 63-148, callers at `mod.rs:1055` and `mod.rs:1268`, 5 test sites at lines 190/204/225/362/450. Caller sweep complete.

### P1.12 — reconcile non-monotonic mtime predicate: NEEDS FIX

- **Issue:** Same `setup_store_for_test` helper does not exist (see P1.11 — actual helper is `open_store`).
- **Issue:** Prompt claims "`filetime` is already a dev-dependency via `tempfile` interactions". Verified against `Cargo.toml:289-297` and `Cargo.lock` — `filetime` is NOT in `[dev-dependencies]` and NOT in the lockfile. Any `use filetime::...` in the new test will fail to compile.
- **Correction (helper):** Use `open_store(&dir.path().join(".cqs"))` per existing tests, after `std::fs::create_dir_all` on the `.cqs` dir.
- **Correction (filetime):** Add `filetime = "0.2"` to `[dev-dependencies]` in `Cargo.toml`. `filetime::FileTime::from_system_time` and `filetime::set_file_mtime` are stable across 0.2.x.
- Current code at `src/cli/watch/reconcile.rs:108-127` matches verbatim. Predicate change `disk > stored` → `disk != stored` correctly addresses the non-monotonic-mtime case the audit identifies.

---

**Tally:** 7 VERIFIED, 5 NEEDS FIX (P1.4, P1.6, P1.9, P1.11, P1.12).

**Most concerning issue:** **P1.4 — PRIVACY/SECURITY incorrect cache-key claim.** The prompt itself is a "fix the lying doc" P1, and its replacement text introduces a *new* lie (claiming `purpose='summary'` rows exist in `embedding_cache.db`). Per the skill's "Lying-doc P1s" rule, the doc must stop lying — applying P1.4 as written would replace one factual error with another. This is the highest-impact verification miss because the audit's whole point of categorizing P1.4 as P1 is that PRIVACY.md is the canonical user-facing surface for "what does cqs store"; a second wrong claim there would be discovered by the next reader as another audit finding.

**Secondary concern:** **P1.11 + P1.12 use a fictional `setup_store_for_test` helper.** Both prompts will fail to compile their regression tests. Easy mechanical fix once flagged but worth catching before the implementation pass dispatches.

# v1.30.1 Audit P2 Fix Prompts

Generated 2026-04-28. Total P2 findings: 32. Distinct fix prompts after grouping: 22.

Grouped bundles:
- **P2-bundle-wait-fresh** absorbs RB-9, EH-V1.30.1-2, OB-V1.30.1-8, TC-HAP-1.30.1-5, TC-ADV-1.30.1-4 (5 findings → 1 prompt). All five share the `wait_for_fresh` poll loop refactor.
- **P2-bundle-reconcile-stat** absorbs EH-V1.30.1-7, TC-ADV-1.30.1-5, TC-ADV-1.30.1-6 (3 findings → 1 prompt). Same `metadata()` arm at `reconcile.rs:116-127`.
- **P2-bundle-watch-status-machine** absorbs OB-V1.30.1-3, TC-HAP-1.30.1-8 (2 findings → 1 prompt). Both about `WatchSnapshot::compute` + transition emission.
- **P2-bundle-eval-gate** absorbs OB-V1.30.1-6, TC-HAP-1.30.1-4, TC-HAP-1.30.1-7 (3 findings → 1 prompt). Same `require_fresh_gate` function.
- **P2-bundle-rb1-rb6** absorbs RB-1, RB-6 (2 findings → 1 prompt). Both about path-string handling in enumerate/reconcile.

Remaining 17 single-issue prompts cover the rest.

---

## P2: P2-bundle-wait-fresh — `wait_for_fresh` papercut bundle (RB-9 + EH-V1.30.1-2 + OB-V1.30.1-8 + TC-HAP-1.30.1-5 + TC-ADV-1.30.1-4)

**Files:** `src/daemon_translate.rs:625-679`, `src/cli/commands/eval/mod.rs:246-264`, plus tests at `src/daemon_translate.rs:1208-1376`
**Effort:** ~90 minutes
**Why:** `wait_for_fresh` is on the hot path of #1182 and bundles five independent papercuts: stringly-typed errors collapse transport/parse failures into `NoDaemon` (wrong advice), `daemon_status` warns at info-level on every connect failure during the 250 ms poll loop (up to 2400 lines/600 s), no exponential backoff, no test for Stale→Fresh transition, no test for daemon-dies-mid-poll. Single refactor pass touches all five surfaces.

### Current code

```rust
// src/daemon_translate.rs:623-678
/// #1182 — Layer 4: outcome of [`wait_for_fresh`].
#[cfg(unix)]
#[derive(Debug, Clone)]
pub enum FreshnessWait {
    Fresh(crate::watch_status::WatchSnapshot),
    Timeout(crate::watch_status::WatchSnapshot),
    NoDaemon(String),
}

#[cfg(unix)]
pub fn wait_for_fresh(cqs_dir: &std::path::Path, wait_secs: u64) -> FreshnessWait {
    let _span = tracing::info_span!("wait_for_fresh", wait_secs).entered();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(wait_secs);
    let poll_interval = std::time::Duration::from_millis(250);

    loop {
        match daemon_status(cqs_dir) {
            Ok(snap) => {
                if snap.is_fresh() {
                    return FreshnessWait::Fresh(snap);
                }
                if std::time::Instant::now() >= deadline {
                    return FreshnessWait::Timeout(snap);
                }
                std::thread::sleep(poll_interval);
            }
            Err(msg) => return FreshnessWait::NoDaemon(msg),
        }
    }
}
```

```rust
// src/daemon_translate.rs:438-441 — connect-stage warn fires every poll
let mut stream = UnixStream::connect(&sock_path).map_err(|e| {
    tracing::warn!(stage = "connect", error = %e, "daemon_status failed");
    format!("connect to {} failed: {e}", sock_path.display())
})?;
```

### Replacement / approach

1. **Distinguish daemon errors at the `daemon_status` layer.** Introduce a `DaemonStatusError` enum with `SocketMissing`, `Transport(String)`, `BadResponse(String)` variants. Update `daemon_status`, `daemon_ping`, `daemon_reconcile` signatures to return `Result<T, DaemonStatusError>` (this fold-in collapses API-V1.30.1-5 too — see separate prompt; keep that finding noted). Demote the connect-failure `tracing::warn!` inside `daemon_status` to `tracing::debug!` so the `wait_for_fresh` poll loop doesn't spam the journal at info level (OB-V1.30.1-8). The caller is responsible for the final-decision warn.

2. **Extend `FreshnessWait`** to mirror the new error shape:

```rust
#[cfg(unix)]
#[derive(Debug, Clone)]
pub enum FreshnessWait {
    Fresh(crate::watch_status::WatchSnapshot),
    Timeout(crate::watch_status::WatchSnapshot),
    /// Socket file missing — the daemon never started.
    NoDaemon(String),
    /// Connect/read/write/timeout — daemon is gone or hung.
    Transport(String),
    /// Envelope/JSON/parse error — daemon answered but garbled.
    BadResponse(String),
}
```

3. **Refactor the poll loop with bounded poll count, exponential backoff, and terminal tracing** (RB-9 + RB-2 + OB-V1.30.1-4 fold-in):

```rust
#[cfg(unix)]
pub fn wait_for_fresh(cqs_dir: &std::path::Path, wait_secs: u64) -> FreshnessWait {
    let _span = tracing::info_span!("wait_for_fresh", wait_secs).entered();
    let start = std::time::Instant::now();
    // Defensive cap: caller should pass a sane budget but a `pub fn` must
    // not panic on `Instant + Duration::from_secs(u64::MAX)` (RB-2).
    let bounded_secs = wait_secs.min(86_400);
    let deadline = start + std::time::Duration::from_secs(bounded_secs);

    let mut poll_interval = std::time::Duration::from_millis(
        crate::limits::freshness_poll_ms_initial(),
    );
    let max_interval = std::time::Duration::from_secs(2);

    loop {
        // RB-9 / AC-V1.30.1-6: deadline-first so a slow daemon timeout
        // can't push us over budget.
        if std::time::Instant::now() >= deadline {
            tracing::info!(
                elapsed_ms = start.elapsed().as_millis() as u64,
                "wait_for_fresh: deadline reached",
            );
            return FreshnessWait::Timeout(crate::watch_status::WatchSnapshot::unknown());
        }

        match daemon_status(cqs_dir) {
            Ok(snap) => {
                if snap.is_fresh() {
                    tracing::info!(
                        elapsed_ms = start.elapsed().as_millis() as u64,
                        modified_files = snap.modified_files,
                        "wait_for_fresh: index reached Fresh",
                    );
                    return FreshnessWait::Fresh(snap);
                }
                if std::time::Instant::now() >= deadline {
                    tracing::info!(
                        elapsed_ms = start.elapsed().as_millis() as u64,
                        modified_files = snap.modified_files,
                        rebuild_in_flight = snap.rebuild_in_flight,
                        "wait_for_fresh: timeout — index still stale",
                    );
                    return FreshnessWait::Timeout(snap);
                }
                std::thread::sleep(poll_interval);
                poll_interval = (poll_interval * 2).min(max_interval);
            }
            Err(DaemonStatusError::SocketMissing(msg)) => {
                tracing::info!(error = %msg, "wait_for_fresh: daemon socket missing");
                return FreshnessWait::NoDaemon(msg);
            }
            Err(DaemonStatusError::Transport(msg)) => {
                tracing::info!(error = %msg, "wait_for_fresh: transport failure");
                return FreshnessWait::Transport(msg);
            }
            Err(DaemonStatusError::BadResponse(msg)) => {
                tracing::info!(error = %msg, "wait_for_fresh: malformed daemon response");
                return FreshnessWait::BadResponse(msg);
            }
        }
    }
}
```

4. **Update the eval gate's `require_fresh_gate`** at `src/cli/commands/eval/mod.rs:246-264` to give different advice per variant (EH-V1.30.1-2):

```rust
match cqs::daemon_translate::wait_for_fresh(&cqs_dir, budget_secs) {
    FreshnessWait::Fresh(_) => Ok(()),
    FreshnessWait::Timeout(snap) => anyhow::bail!(
        "watch index is still stale after {budget_secs}s wait \
         (modified_files={}, pending_notes={}, rebuild_in_flight={}); \
         wait longer with --require-fresh-secs N or skip with --no-require-fresh",
        snap.modified_files, snap.pending_notes, snap.rebuild_in_flight,
    ),
    FreshnessWait::NoDaemon(msg) => anyhow::bail!(
        "watch daemon not reachable: {msg}\n\n\
         Eval --require-fresh requires a running `cqs watch --serve`. Start it \
         (`systemctl --user start cqs-watch`) or rerun with `--no-require-fresh`."
    ),
    FreshnessWait::Transport(msg) => anyhow::bail!(
        "watch daemon transport error: {msg}\n\n\
         The daemon socket exists but isn't responding. Check daemon health: \
         `journalctl --user -u cqs-watch -n 50` and consider \
         `systemctl --user restart cqs-watch`."
    ),
    FreshnessWait::BadResponse(msg) => anyhow::bail!(
        "watch daemon returned malformed response: {msg}\n\n\
         The daemon answered but the response was unparseable — likely a \
         version skew. Restart cqs-watch and retry."
    ),
}
```

5. **Add `crate::limits::freshness_poll_ms_initial()`** reading `CQS_FRESHNESS_POLL_MS` (default 100, floor 25, ceiling 5000). Folds in SHL-V1.30-2 if convenient.

6. **Add three tests** at `src/daemon_translate.rs::tests` (mirroring the existing `wait_for_fresh_returns_fresh_on_first_poll` mock pattern):
   - `wait_for_fresh_returns_fresh_after_two_stale_polls` — TC-HAP-1.30.1-5: `UnixListener` accepts 3 connections, writes Stale envelope twice then Fresh, assert `FreshnessWait::Fresh(_)` and elapsed ≥ initial poll interval.
   - `wait_for_fresh_returns_transport_when_daemon_dies_mid_poll` — TC-ADV-1.30.1-4: listener accepts first connection (returns Stale), then closes; assert subsequent return is `Transport(_)` not `NoDaemon` (socket file still exists but connection refused).
   - `daemon_status_returns_bad_response_on_malformed_envelope` — TC-ADV-1.30.1-4: listener writes `{"status":"ok","output":` and closes; assert `Err(BadResponse(_))` distinguishable from socket-missing case.

### Verification

- `cargo build --features gpu-index` succeeds.
- `cargo test --features gpu-index --lib daemon_translate -- wait_for_fresh` passes the three new tests.
- Run `cqs eval --require-fresh` against a daemon that's hung (`kill -STOP $(pgrep -f cqs-watch)`) — expect a `Transport(...)` bail message within `--require-fresh-secs`, not `NoDaemon`.
- Verify journal output during a long wait: `journalctl --user -u cqs-watch -f` shows at most one info line per poll outcome, never a flood of connect-warn lines.

---

## P2: P2-bundle-reconcile-stat — Reconcile `metadata()` error swallowed (EH-V1.30.1-7 + TC-ADV-1.30.1-5 + TC-ADV-1.30.1-6)

**Files:** `src/cli/watch/reconcile.rs:116-132`, plus parallel pattern at `src/cli/watch/reindex.rs:501-509`
**Effort:** ~45 minutes
**Why:** Reconcile's `(Some(_), None)` and `disk > stored` arms swallow stat failures and clock skew silently. Permission-denied files or backwards-clock files quietly stay stale forever. No tracing, no test seeds the unreadable / future-mtime cases.

### Current code

```rust
// src/cli/watch/reconcile.rs:116-132
let disk_mtime = match lookup_path.metadata().and_then(|m| m.modified()) {
    Ok(t) => t
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(cqs::duration_to_mtime_millis),
    Err(_) => None,
};
let needs_reindex = match (stored_mtime, disk_mtime) {
    (Some(stored), Some(disk)) => disk > *stored,
    (None, _) => true,        // legacy/null stored mtime
    (Some(_), None) => false, // can't read disk mtime → leave to GC
};
if needs_reindex && pending_files.insert(rel.clone()) {
    modified += 1;
    queued += 1;
}
```

### Replacement / approach

```rust
// 1. Capture the stat error so we can warn on it.
let disk_mtime = match lookup_path.metadata().and_then(|m| m.modified()) {
    Ok(t) => t
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(cqs::duration_to_mtime_millis),
    Err(e) => {
        // EH-V1.30.1-7 / TC-ADV-1.30.1-6: surface stat failures so the
        // operator can distinguish permission-denied files from
        // genuinely-missing ones. Debug level keeps the journal clean
        // for the common transient-AV-scan-on-WSL case but still
        // searchable via `journalctl --priority=debug`.
        tracing::debug!(
            path = %lookup_path.display(),
            error = %e,
            "Reconcile: stat failed, leaving file to GC",
        );
        None
    }
};

let needs_reindex = match (stored_mtime, disk_mtime) {
    (Some(stored), Some(disk)) => {
        // TC-ADV-1.30.1-5: detect clock-skew (stored > disk) and warn so
        // the operator sees the corruption instead of silently dropping
        // the file from reconcile forever. Treat as stale (reindex) —
        // the file's content may have changed even if mtime moves
        // backward (git checkout to older commit, for instance).
        if *stored > disk {
            tracing::warn!(
                path = %lookup_path.display(),
                stored_mtime = stored,
                disk_mtime = disk,
                "Reconcile: stored mtime is in the future relative to disk \
                 (clock skew or git checkout to older commit?) — queuing reindex",
            );
            true
        } else {
            disk > *stored
        }
    }
    (None, _) => true,        // legacy/null stored mtime
    (Some(_), None) => false, // can't read disk mtime → leave to GC (debug warning above)
};
if needs_reindex && pending_files.insert(rel.clone()) {
    modified += 1;
    queued += 1;
}
```

Apply the same `tracing::debug!` warning to the parallel arm at `src/cli/watch/reindex.rs:501-509` where `mtime=None` is silently stored.

### Tests to add (in `src/cli/watch/reconcile.rs::tests`)

```rust
#[test]
#[cfg(unix)]
fn reconcile_clock_skew_stored_mtime_in_future_queues_reindex() {
    // Seed a chunk with stored_mtime = now + 1h, write file at now.
    // Assert the file is queued in pending_files (clock-skew detected).
    // Verify the warn fires via tracing_test or capture.
}

#[test]
#[cfg(unix)]
fn reconcile_metadata_err_leaves_file_alone_with_debug_log() {
    // chmod 0 on parent dir, run reconcile, assert file is NOT queued
    // and the debug-level message was emitted (use tracing_test).
}
```

### Verification

- `cargo test --features gpu-index --lib reconcile -- reconcile_clock_skew reconcile_metadata_err`.
- Manually: `chmod 000 /path/inside/repo`, restart daemon, `journalctl --user -u cqs-watch -f --priority=debug` shows the path-with-error line. Restore perms.

---

## P2: P2-bundle-watch-status-machine — Silent state transitions (OB-V1.30.1-3 + TC-HAP-1.30.1-8)

**Reframed during verification:** original prompt's two proposed compute_* tests duplicated the existing `rebuild_dominates_over_stale_files` test at `src/watch_status.rs:278`. Both have been dropped. The transition-log emission fix (the load-bearing piece) is unchanged. The follow-on test that captures the transition log requires a non-existing `tracing-test` dev-dep — left as an explicit (a)-vs-(b) decision below. Both bundled finding IDs (OB-V1.30.1-3, TC-HAP-1.30.1-8) remain substantively covered: OB-V1.30.1-3 by the prev/next-compare emission, TC-HAP-1.30.1-8 by the existing 6 `compute_*` tests already pinning all four states.

**Files:** `src/cli/watch/mod.rs:149-185`, `src/watch_status.rs:195-224`, tests in `src/watch_status.rs::tests`
**Effort:** ~30 minutes (was ~45 — test additions dropped)
**Why:** `publish_watch_snapshot` overwrites the snapshot every 100 ms with no transition logging — operators see no journal trail for Fresh↔Stale↔Rebuilding flips. (Original Why also claimed "Rebuilding and Unknown have no test through compute()" — that was wrong; the existing 6 `compute_*` tests already cover all four states including Rebuilding via `rebuild_dominates_over_stale_files` at line 278.)

### Current code

```rust
// src/cli/watch/mod.rs:149-185
fn publish_watch_snapshot(
    handle: &cqs::watch_status::SharedWatchSnapshot,
    state: &WatchState,
    index_path: &std::path::Path,
) {
    let last_synced_at = std::fs::metadata(index_path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64);
    let delta_saturated = state
        .pending_rebuild
        .as_ref()
        .map(|p| p.delta_saturated)
        .unwrap_or(false);
    let snap = cqs::watch_status::WatchSnapshot::compute(cqs::watch_status::WatchSnapshotInput {
        // ...
    });
    match handle.write() {
        Ok(mut guard) => *guard = snap,
        Err(poisoned) => {
            tracing::warn!("watch_snapshot RwLock poisoned — recovering and continuing to publish");
            *poisoned.into_inner() = snap;
        }
    }
}
```

### Replacement / approach

1. **Compare prev and next state under the write lock** and emit a single info-level transition event:

```rust
fn publish_watch_snapshot(
    handle: &cqs::watch_status::SharedWatchSnapshot,
    state: &WatchState,
    index_path: &std::path::Path,
) {
    // ... existing snapshot-build code ...

    let prev_state;
    match handle.write() {
        Ok(mut guard) => {
            prev_state = guard.state;
            *guard = snap.clone(); // clone to log after lock release
        }
        Err(poisoned) => {
            tracing::warn!("watch_snapshot RwLock poisoned — recovering and continuing to publish");
            let mut guard = poisoned.into_inner();
            prev_state = guard.state;
            *guard = snap.clone();
        }
    }

    if prev_state != snap.state {
        tracing::info!(
            prev = prev_state.as_str(),
            next = snap.state.as_str(),
            modified_files = snap.modified_files,
            rebuild_in_flight = snap.rebuild_in_flight,
            dropped_this_cycle = snap.dropped_this_cycle,
            "watch state transition",
        );
    }
}
```

(Note: `WatchSnapshot` already derives `Clone`. `FreshnessState` derives `Copy` per `watch_status.rs:51`.)

2. **Drop both originally-proposed `compute_*` tests** — they duplicated the existing `rebuild_dominates_over_stale_files` test at `watch_status.rs:278` which already pins `rebuild_in_flight=true, pending_files=5 -> Rebuilding`. The Why-section claim "Rebuilding has no test through compute()" was wrong; the existing 6 compute_* tests cover Fresh, Stale (via pending_files / pending_notes / dropped_events), Rebuilding, and Unknown.

The remaining coverage question is whether `publish_watch_snapshot` actually emits the transition log on a state flip. There is no existing log-capture infrastructure in this codebase (verified: no `tracing-test` dev-dep in `Cargo.toml`, no `logs_contain` helper anywhere in `src/` or `tests/`). Two options:

   **(a) Skip the transition-emission test** and rely on the manual journalctl smoke step below — the OB-V1.30.1-3 finding is substantively addressed by the prev/next compare under the write lock; an automated assertion adds little because the only failure mode is "didn't emit", which is hard to regress accidentally given the explicit `if prev_state != snap.state` branch.

   **(b) If automated coverage matters,** add `tracing-test = "0.2"` to `[dev-dependencies]` in `Cargo.toml` and write a `#[tracing_test::traced_test]` test that drives `publish_watch_snapshot` twice with different states and asserts `logs_contain("watch state transition")`. Mark this as scope-creep relative to OB-V1.30.1-3 and split into a separate prompt if it doesn't fit the bundle's effort budget.

Recommend (a) for this bundle — the transition-emission code path is small and reading the diff is sufficient verification.

### Verification

- `cargo test --features gpu-index --lib watch_status` (existing 6 compute_* tests still pass — Rebuilding precedence already covered by `rebuild_dominates_over_stale_files`).
- Manually trigger reindex, observe one `watch state transition prev=fresh next=stale` line in `journalctl`, then `prev=stale next=rebuilding`, then `prev=rebuilding next=fresh`. No spam between transitions.

---

## P2: P2-bundle-eval-gate — `require_fresh_gate` invisible + untested (OB-V1.30.1-6 + TC-HAP-1.30.1-4 + TC-HAP-1.30.1-7)

**Files:** `src/cli/commands/eval/mod.rs:219-275`, plus new `tests/cli_eval_freshness_gate_test.rs`
**Effort:** ~60 minutes
**Why:** `require_fresh_gate` is the central #1182 control point but lacks entry/exit tracing, the function itself is never called by any test, and every integration test bypasses it via `CQS_EVAL_REQUIRE_FRESH=0`.

### Current code

```rust
// src/cli/commands/eval/mod.rs:219-275
fn require_fresh_gate(no_require_fresh_flag: &bool, wait_secs: u64) -> Result<()> {
    if *no_require_fresh_flag {
        tracing::info!("Eval freshness gate disabled via --no-require-fresh");
        return Ok(());
    }
    if env_disables_freshness_gate() {
        tracing::info!("Eval freshness gate disabled via CQS_EVAL_REQUIRE_FRESH");
        eprintln!(
            "[eval] CQS_EVAL_REQUIRE_FRESH disables the freshness gate; running against current index"
        );
        return Ok(());
    }
    // ... wait_for_fresh dispatch ...
}
```

### Replacement / approach

1. **Wrap the function body in a span and emit terminal events**:

```rust
fn require_fresh_gate(no_require_fresh_flag: &bool, wait_secs: u64) -> Result<()> {
    let _span = tracing::info_span!("require_fresh_gate", wait_secs).entered();
    let start = std::time::Instant::now();

    if *no_require_fresh_flag {
        tracing::info!(
            outcome = "bypass_flag",
            "require_fresh_gate: disabled via --no-require-fresh",
        );
        return Ok(());
    }
    if env_disables_freshness_gate() {
        tracing::info!(
            outcome = "bypass_env",
            "require_fresh_gate: disabled via CQS_EVAL_REQUIRE_FRESH",
        );
        eprintln!(
            "[eval] CQS_EVAL_REQUIRE_FRESH disables the freshness gate; running against current index"
        );
        return Ok(());
    }

    #[cfg(unix)]
    {
        use cqs::daemon_translate::FreshnessWait;
        let root = crate::cli::find_project_root();
        let cqs_dir = cqs::resolve_index_dir(&root);
        let budget_secs = wait_secs.min(600);

        eprintln!(
            "[eval] checking watch-mode freshness (--no-require-fresh to skip; CQS_EVAL_REQUIRE_FRESH=0 in env)"
        );

        let result = cqs::daemon_translate::wait_for_fresh(&cqs_dir, budget_secs);
        let elapsed_ms = start.elapsed().as_millis() as u64;
        match &result {
            FreshnessWait::Fresh(snap) => tracing::info!(
                outcome = "fresh",
                elapsed_ms,
                modified_files = snap.modified_files,
                "require_fresh_gate: resolved",
            ),
            FreshnessWait::Timeout(snap) => tracing::info!(
                outcome = "timeout",
                elapsed_ms,
                modified_files = snap.modified_files,
                "require_fresh_gate: resolved",
            ),
            FreshnessWait::NoDaemon(_) => tracing::info!(
                outcome = "no_daemon",
                elapsed_ms,
                "require_fresh_gate: resolved",
            ),
            // ... new variants from P2-bundle-wait-fresh ...
        }
        // ... existing bail/return logic ...
    }
    // ... non-unix branch unchanged ...
}
```

2. **Make `require_fresh_gate` and `env_disables_freshness_gate` `pub(crate)`** if not already, so the new test crate can call them.

3. **Add unit tests** in `src/cli/commands/eval/mod.rs::tests`:

```rust
#[test]
fn require_fresh_gate_no_require_fresh_flag_returns_ok_without_daemon() {
    // Just call it — no socket needed because the flag short-circuits
    // before any daemon call.
    let result = require_fresh_gate(&true, 5);
    assert!(result.is_ok());
}

#[test]
#[serial_test::serial(cqs_eval_require_fresh_env)]
fn require_fresh_gate_env_disable_returns_ok_without_daemon() {
    // SAFETY: serial test guards env mutation
    unsafe {
        std::env::set_var("CQS_EVAL_REQUIRE_FRESH", "0");
    }
    let result = require_fresh_gate(&false, 5);
    unsafe {
        std::env::remove_var("CQS_EVAL_REQUIRE_FRESH");
    }
    assert!(result.is_ok());
}
```

4. **Add an integration test** at `tests/cli_eval_freshness_gate_test.rs`:
   - Spin up the same `UnixListener` mock as `wait_for_fresh_returns_fresh_on_first_poll`.
   - Point `XDG_RUNTIME_DIR` at the tempdir hosting it.
   - Run `cqs eval` *without* `CQS_EVAL_REQUIRE_FRESH=0`.
   - Assert exit 0 within 10 s, the `[eval] checking watch-mode freshness` line on stderr, and a non-zero `recall_at_5` in the report JSON.
   - Gate behind `slow-tests` like other CLI tests.

### Verification

- `cargo test --features gpu-index --lib eval -- require_fresh_gate`.
- `cargo test --features gpu-index,slow-tests --test cli_eval_freshness_gate_test`.
- `journalctl` shows `outcome=fresh elapsed_ms=...` or `outcome=timeout` per eval invocation.

---

## P2: P2-bundle-rb1-rb6 — Path string mangling in reconcile + enumerate (RB-1 + RB-6)

**Files:** `src/cli/watch/reconcile.rs:99`, `src/lib.rs:680-685`
**Effort:** ~45 minutes
**Why:** Two parallel "lossy path string" papercuts. Reconcile builds a UTF-8 lookup key via `to_string_lossy().replace('\\', "/")` — non-UTF-8 paths get U+FFFD substitution that won't match the indexer's own lossy conversion, causing permanent reindex storms. `enumerate_files` uses `unwrap_or(&path)` after a failed `strip_prefix` — on case-insensitive Windows / NTFS this silently leaks absolute paths into the relative-path workflow.

### Current code

```rust
// src/cli/watch/reconcile.rs:95-99
for rel in disk_files {
    // Stored origins are typically relative; normalize to forward
    // slashes for cross-platform matching parity with the rest of the
    // store layer.
    let origin = rel.to_string_lossy().replace('\\', "/");
```

```rust
// src/lib.rs:680-685
if path.starts_with(&root) {
    Some(path.strip_prefix(&root).unwrap_or(&path).to_path_buf())
} else {
    tracing::warn!(path = %e.path().display(), "Skipping path outside project");
    None
}
```

### Replacement / approach

```rust
// src/cli/watch/reconcile.rs:95-99 — skip non-UTF-8 paths up front with warn
for rel in disk_files {
    let origin = match rel.to_str() {
        Some(s) => s.replace('\\', "/"),
        None => {
            // RB-1: non-UTF-8 path bytes don't round-trip through
            // `to_string_lossy()` consistently with the indexer's own
            // lossy conversion. Skipping with a warn is strictly
            // better than re-queuing the file forever every reconcile
            // cycle (~30 s) on WSL `/mnt/c/`.
            tracing::warn!(
                path = %rel.display(),
                "reconcile: skipping non-UTF-8 path (will not be indexed until renamed)",
            );
            continue;
        }
    };
```

```rust
// src/lib.rs:680-685 — surface case-insensitive-FS disagreement
if path.starts_with(&root) {
    match path.strip_prefix(&root) {
        Ok(rel) => Some(rel.to_path_buf()),
        Err(_) => {
            // RB-6: starts_with said yes but strip_prefix said no —
            // case-insensitive filesystem (NTFS/HFS+) where byte
            // comparison sees `Cqs` vs `cqs` as different. Skip and
            // warn so the operator sees the disagreement; the
            // alternative (unwrap_or(&path)) silently leaks an
            // absolute path into the rel workflow and breaks every
            // downstream lookup.
            tracing::warn!(
                path = %path.display(),
                root = %root.display(),
                "enumerate_files: starts_with passed but strip_prefix failed \
                 (case-insensitive fs?) — skipping",
            );
            None
        }
    }
} else {
    tracing::warn!(path = %e.path().display(), "Skipping path outside project");
    None
}
```

### Verification

- `cargo build --features gpu-index`.
- Linux test: create file with non-UTF-8 bytes (`touch $(printf 'foo\xff.rs')`), run reconcile, observe warn line + file absent from `pending_files`.
- Windows test (manual): in a repo at `C:\Projects\cqs`, force a path with mismatched case via junction; run `cqs index` and check for the warn rather than absolute paths in the chunk store.

---

## P2: EH-V1.30.1-1 — Parse failure leaves file with stale chunks AND no mtime update — reconciles forever

**Files:** `src/cli/watch/reindex.rs:255-314`
**Effort:** ~45 minutes
**Why:** When `parser.parse_file_all_with_chunk_calls` returns `Err`, the watch loop emits `vec![]` for that file. The previous chunks stay as ghosts (no `upsert_*_and_prune` call), AND `chunks.source_mtime` is never updated, so reconcile keeps classifying the file MODIFIED on every tick — unbounded reindex-fail-warn loop until the syntax error is fixed.

### Current code

```rust
// src/cli/watch/reindex.rs:308-314
                    file_chunks
                }
                Err(e) => {
                    tracing::warn!(path = %abs_path.display(), error = %e, "Failed to parse file");
                    vec![]
                }
            }
```

### Replacement / approach

1. On parse failure, still touch `chunks.source_mtime` for this file so reconcile sees `disk == stored` and stops requeuing.
2. Optionally also queue the file's chunks for deletion (matching the deleted-file branch at line 244-253) so a syntax-error file doesn't keep ghost results in search.
3. Add a `parse_errors` counter to `WatchState` and surface it in `WatchSnapshot` so `cqs status --watch-fresh` shows stuck files.

```rust
                Err(e) => {
                    tracing::warn!(
                        path = %abs_path.display(),
                        error = %e,
                        "Failed to parse file — touching mtime to break reconcile loop",
                    );
                    // EH-V1.30.1-1: refresh source_mtime so reconcile.rs:124
                    // sees disk == stored and stops re-queuing this file every
                    // 30 s. The file stays unindexed (its previous chunks may
                    // still serve from the index as ghosts — that's accepted
                    // until the user fixes the syntax error and triggers a
                    // re-parse). The mtime touch is the load-bearing piece.
                    if let Ok(meta) = std::fs::metadata(&abs_path) {
                        if let Ok(disk_mtime) = meta.modified() {
                            if let Ok(d) = disk_mtime.duration_since(std::time::UNIX_EPOCH) {
                                let mtime_ms = cqs::duration_to_mtime_millis(d);
                                if let Err(touch_err) =
                                    store.touch_source_mtime(rel_path, mtime_ms)
                                {
                                    tracing::warn!(
                                        path = %rel_path.display(),
                                        error = %touch_err,
                                        "Failed to touch source_mtime for parse-failed file",
                                    );
                                }
                            }
                        }
                    }
                    vec![]
                }
```

`Store::touch_source_mtime(&Path, i64)` does not exist yet (verified — `grep -rn touch_source_mtime src/` returns nothing). Add it as a thin `UPDATE chunks SET source_mtime = ? WHERE origin = ?` wrapper next to `delete_by_origin` (`src/store/chunks/crud.rs:614`).

**Critical:** the helper MUST call `crate::normalize_path(origin)` before binding to `WHERE origin = ?` — see `delete_by_origin` at `src/store/chunks/crud.rs:614-616` for the canonical pattern (`let origin_str = crate::normalize_path(origin);`). Without normalization the path-vs-origin string format won't match what the indexer stored (Windows `\\` vs Unix `/` separator, leading `./`, etc.) and the UPDATE will silently affect zero rows — defeating the entire fix.

```rust
/// Refresh source_mtime on every chunk for `origin` without touching content.
/// Used by the watch loop's parse-failure path so reconcile sees disk == stored
/// and stops re-queuing files that the parser cannot handle.
pub fn touch_source_mtime(&self, origin: &Path, mtime_ms: i64) -> Result<u32, StoreError> {
    let _span = tracing::debug_span!("touch_source_mtime", origin = %origin.display()).entered();
    let origin_str = crate::normalize_path(origin); // load-bearing — must match indexer's key format

    self.rt.block_on(async {
        let (_guard, mut tx) = self.begin_write().await?;
        let result = sqlx::query("UPDATE chunks SET source_mtime = ?1 WHERE origin = ?2")
            .bind(mtime_ms)
            .bind(&origin_str)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(result.rows_affected() as u32)
    })
}
```

### Verification

- `cargo build --features gpu-index`.
- `cargo test --features gpu-index --lib store -- touch_source_mtime` (add a unit test that asserts `rows_affected > 0` for a chunk inserted with the indexer's normalized origin format).
- Smoke: introduce a syntax error in a watched file (`echo 'fn foo(' > foo.rs`); observe one warn line then no further requeue spam in `journalctl`. Fix the syntax error, file gets reindexed.

---

## P2: EH-V1.30.1-8 — watch reindex failure leaves HNSW dirty without observability

**Reframed during verification:** original title said `try_init_embedder Err leaves HNSW dirty`, but `try_init_embedder` returns `Option<Embedder>` (`None`, not `Err`) and the actual scope is the dirty-flag path at `events.rs:178-185` plus the `reindex_files` Err arm at line 419-421. Title and Why now describe the real symptom: a failed reindex cycle (SQLite busy, OOM, etc.) sets the dirty flag, the `Err` branch only emits a warn, and `clear_hnsw_dirty_with_retry` is never reached so the flag stays set indefinitely.

**Files:** `src/cli/watch/events.rs:178-185` (dirty-flag set), `src/cli/watch/events.rs:419-421` (reindex_files Err arm)
**Effort:** ~30 minutes
**Why:** `set_hnsw_dirty(true)` runs at `events.rs:178-185` before every reindex attempt. When `reindex_files` returns `Err` (e.g. SQLite busy, panic, OOM), the match arm at line 419-421 just emits `warn!("Reindex error")` and returns — the matching `clear_hnsw_dirty_with_retry` (at line 364, only on the Ok path) never fires. There's no operator visibility into "we've been dirty for N cycles in a row" — each failed cycle just emits a warn and moves on.

### Current code

```rust
// src/cli/watch/events.rs:178-185
    if let Err(e) = store.set_hnsw_dirty(cqs::HnswKind::Enriched, true) {
        tracing::warn!(error = %e, "Cannot set enriched HNSW dirty flag — skipping reindex to prevent stale index on crash");
        return;
    }
    if let Err(e) = store.set_hnsw_dirty(cqs::HnswKind::Base, true) {
        tracing::warn!(error = %e, "Cannot set base HNSW dirty flag — skipping reindex to prevent stale index on crash");
        return;
    }
```

### Replacement / approach

1. Add a `consecutive_dirty_cycles: u32` field to `WatchState`.
2. In the `reindex_files` `Err` arm (or wherever the dirty-clear call lives), increment the counter on failure and reset to 0 on the success path right after `clear_hnsw_dirty_with_retry`.
3. After increment, if `consecutive_dirty_cycles >= 3` emit a louder warn:

```rust
state.consecutive_dirty_cycles = state.consecutive_dirty_cycles.saturating_add(1);
if state.consecutive_dirty_cycles >= 3 {
    tracing::warn!(
        cycles = state.consecutive_dirty_cycles,
        "HNSW has been dirty for {N} cycles — search may serve stale results until \
         reindex completes; check journalctl for prior errors",
    );
}
```

4. Surface the count in `WatchSnapshot` (add `consecutive_dirty_cycles: u32` field, plumb through `WatchSnapshotInput`).

### Verification

- `cargo build --features gpu-index`.
- `cargo test --features gpu-index --lib watch_status` for new field's serde round-trip.
- Manual: simulate failure by `chmod -w` on the index dir, confirm 3+ cycles produce the louder warn.

---

## P2: RB-9 — `wait_for_fresh` infinite poll loop on slow daemon (covered by P2-bundle-wait-fresh)

**See:** P2-bundle-wait-fresh above. RB-9's exponential-backoff and bounded-poll-count fixes are folded into the bundle.

---

## P2: RB-1 — `to_string_lossy()` on path keys (covered by P2-bundle-rb1-rb6)

**See:** P2-bundle-rb1-rb6 above.

---

## P2: RB-6 — `enumerate_files` `unwrap_or(&path)` (covered by P2-bundle-rb1-rb6)

**See:** P2-bundle-rb1-rb6 above.

---

## P2: AC-V1.30.1-3 — BFS `bfs_expand` cap check skips score-bump for already-visited neighbors

**Files:** `src/gather.rs:357-381`
**Effort:** ~30 minutes
**Why:** Inner cap check at line 357 fires *before* the visited-neighbor score-bump at line 366-377. When the cap fires partway through a high-fanout node's neighbors, downstream nodes that *would* have had their score bumped to a higher value keep their stale lower score. Quality degrades silently at the cap boundary.

### Current code

```rust
// src/gather.rs:341-383
    while let Some((name, depth)) = queue.pop_front() {
        if depth >= opts.expand_depth {
            continue;
        }
        if name_scores.len() >= opts.max_expanded_nodes && visited.len() > initial_size {
            expansion_capped = true;
            break;
        }

        let neighbors = get_neighbors(graph, &name, opts.direction);
        let base_score = name_scores
            .get(name.as_ref())
            .map(|(s, _)| *s)
            .unwrap_or(0.5);
        let new_score = base_score * opts.decay_factor;
        for neighbor in neighbors {
            if name_scores.len() >= opts.max_expanded_nodes {
                expansion_capped = true;
                break;
            }
            if !visited.contains(&neighbor) {
                visited.insert(Arc::clone(&neighbor));
                let key: String = neighbor.to_string();
                name_scores.insert(key, (new_score, depth + 1));
                queue.push_back((neighbor, depth + 1));
            } else if let Some(existing) = name_scores.get_mut(neighbor.as_ref()) {
                if new_score > existing.0 {
                    existing.0 = new_score;
                    existing.1 = existing.1.min(depth + 1);
                }
            }
        }
        if expansion_capped {
            break;
        }
    }
```

### Replacement / approach

```rust
        for neighbor in neighbors {
            if !visited.contains(&neighbor) {
                // Cap is on `name_scores.len()`, only checked for new
                // insertions. Already-visited bumps below don't grow
                // the map, so let them through unconditionally.
                if name_scores.len() >= opts.max_expanded_nodes {
                    expansion_capped = true;
                    break;
                }
                visited.insert(Arc::clone(&neighbor));
                let key: String = neighbor.to_string();
                name_scores.insert(key, (new_score, depth + 1));
                queue.push_back((neighbor, depth + 1));
            } else if let Some(existing) = name_scores.get_mut(neighbor.as_ref()) {
                // AC-V1.30.1-3: always run the score-bump even when the
                // cap is reached; the bump doesn't grow `name_scores`.
                if new_score > existing.0 {
                    existing.0 = new_score;
                    existing.1 = existing.1.min(depth + 1);
                }
            }
        }
```

### Tests to add

```rust
#[test]
fn bfs_expand_score_bump_runs_when_cap_reached() {
    // Construct a graph where two seeds both expand into a shared third
    // node N. Set max_expanded_nodes such that the cap fires after the
    // first seed's expansion. Assert N's score is the max of the two
    // seeds' decayed scores, not whichever the BFS reached first.
}
```

### Verification

- `cargo test --features gpu-index --lib gather -- bfs_expand`.

---

## P2: AC-V1.30.1-9 — `daemon_socket_path` uses `DefaultHasher`

**Files:** `src/daemon_translate.rs:174-205`
**Effort:** ~30 minutes
**Why:** `DefaultHasher` uses Rust-version-dependent SipHash. A `cargo update` of std could change socket names, breaking systemd `cqs-watch` units that hardcode a previous socket path. Codebase already depends on `blake3` for content hashing.

### Current code

```rust
// src/daemon_translate.rs:174-205
#[cfg(unix)]
pub fn daemon_socket_path(cqs_dir: &std::path::Path) -> std::path::PathBuf {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::path::PathBuf;

    let sock_dir = match std::env::var_os("XDG_RUNTIME_DIR") {
        // ... unchanged ...
    };
    let sock_name = format!("cqs-{:x}.sock", {
        let mut h = DefaultHasher::new();
        cqs_dir.hash(&mut h);
        h.finish()
    });
    sock_dir.join(sock_name)
}
```

### Replacement / approach

```rust
#[cfg(unix)]
pub fn daemon_socket_path(cqs_dir: &std::path::Path) -> std::path::PathBuf {
    use std::path::PathBuf;

    let sock_dir = match std::env::var_os("XDG_RUNTIME_DIR") {
        // ... unchanged ...
    };
    // AC-V1.30.1-9: BLAKE3 is stable across Rust versions — important
    // because systemd unit files hardcode the resulting socket path.
    // Truncate to 8 hex bytes (16 chars) — collision probability for
    // 100 projects is ~1e-15.
    let canonical_path_bytes = cqs_dir.as_os_str().as_encoded_bytes();
    let hash = blake3::hash(canonical_path_bytes);
    let truncated = &hash.as_bytes()[..8];
    let sock_name = format!(
        "cqs-{}.sock",
        truncated.iter().map(|b| format!("{:02x}", b)).collect::<String>()
    );
    sock_dir.join(sock_name)
}
```

### Migration note (one-time operator action)

This is a wire-format change to the socket name, not just an internal refactor. The existing `DefaultHasher` produces variable-length unpadded hex (`cqs-deadbeef.sock`), the BLAKE3 + 8-byte truncation produces fixed 16-char hex (`cqs-1234567890abcdef.sock`). For a given `cqs_dir` the new hash is **different** from the old — operators with a running `cqs-watch` systemd unit must:

```bash
systemctl --user restart cqs-watch
# old socket abandoned; daemon binds new path; CLI auto-discovers via XDG_RUNTIME_DIR
```

CLI auto-connects (the wrapper at `src/cli/files.rs:26` calls the same path resolver), so once the daemon binds the new name, queries find it. The transition window is one restart, no config files to edit. Ship this in the same release as the rest of v1.30.x or operators will see "daemon not responding" until they restart.

### Verification

- `cargo build --features gpu-index`.
- `cargo test --features gpu-index --lib daemon_translate -- daemon_socket_path` passes (existing tests should still hash to deterministic outputs).
- Pin a regression test: hash for a known input path (e.g. `/tmp/foo`) returns a known fixed string.
- After the upgrade, smoke-test: `systemctl --user restart cqs-watch && cqs status --json` should return a non-error envelope within 2 seconds.

---

## P2: AC-V1.30.1-10 — `incremental_count = 0` reset on idle-clear loses delta context

**Files:** `src/cli/watch/mod.rs:1175-1182`
**Effort:** ~30 minutes
**Why:** When the watch loop has been idle ~5 minutes, it clears the embedder session AND resets `state.incremental_count = 0` AND drops `state.hnsw_index`. The counter reset is the bug: counter should track "incremental inserts since last full rebuild", not "since last embedder-session reset". Resetting on idle understates delta size and delays the next threshold-driven rebuild.

### Current code

```rust
// src/cli/watch/mod.rs:1175-1182
                    if cycles_since_clear >= 3000 {
                        if let Some(emb) = shared_embedder.get() {
                            emb.clear_session();
                        }
                        state.hnsw_index = None;
                        state.incremental_count = 0;
                        cycles_since_clear = 0;
                    }
```

### Replacement / approach

```rust
                    if cycles_since_clear >= 3000 {
                        if let Some(emb) = shared_embedder.get() {
                            emb.clear_session();
                        }
                        // AC-V1.30.1-10: do NOT reset incremental_count
                        // on idle-clear. The counter's contract is
                        // "incremental inserts since last full rebuild";
                        // a 5-minute idle hasn't changed the on-disk
                        // delta. Resetting here means the next file
                        // event starts the threshold timer from scratch
                        // and understates delta size, delaying the
                        // rebuild that should fire on accumulated drift.
                        state.hnsw_index = None;
                        cycles_since_clear = 0;
                    }
```

### Test to add

```rust
#[test]
fn incremental_count_persists_across_idle_clear() {
    // Set state.incremental_count = threshold - 1, simulate 3000-tick
    // idle, push one file event, assert a full rebuild fires (not an
    // incremental insert). Mock the rebuild via a callback counter.
}
```

### Verification

- `cargo build --features gpu-index`.
- `cargo test --features gpu-index --lib watch -- incremental_count`.

---

## P2: API-V1.30.1-1 — `cqs status --wait` emits success envelope but exits 1 on timeout

**Reframed during verification:** original prompt referenced `error_codes::TIMEOUT` which does not exist — the real `error_codes` module (`src/cli/json_envelope.rs:125-139`) only defines `NOT_FOUND`, `INVALID_INPUT`, `PARSE_ERROR`, `IO_ERROR`, `INTERNAL`. Replacement now extends `ErrorCode` enum with a `Timeout` variant + `error_codes::TIMEOUT` constant (the cleanest fix, in keeping with the existing single-source-of-truth pattern at `src/cli/json_envelope.rs:80-138`).

**Files:** `src/cli/commands/infra/status.rs:85-90`, `src/cli/json_envelope.rs:80-139` (extension)
**Effort:** ~30 minutes
**Why:** On `Timeout`, the command emits the success-envelope JSON via `emit_snapshot` *and* exits 1. JSON consumers see `{"status":"ok",...}` then a non-zero exit code — contradicts the contract that error envelopes have `status:"err"`. A scripted consumer parsing the envelope alone gets the wrong answer.

### Current code

```rust
// src/cli/commands/infra/status.rs:85-90
            cqs::daemon_translate::FreshnessWait::Timeout(snap) => {
                emit_snapshot(&snap, json)?;
                // Budget expired before fresh — surface as exit 1
                // so scripts can distinguish "fresh" from "timed
                // out still stale".
                std::process::exit(1);
            }
```

### Replacement / approach

**Step 1.** Extend the `ErrorCode` enum + `error_codes` module in `src/cli/json_envelope.rs`. The current set covers `NotFound`, `InvalidInput`, `ParseError`, `IoError`, `Internal`. Add a `Timeout` variant alongside:

```rust
// src/cli/json_envelope.rs:80-93 — add to the enum
pub enum ErrorCode {
    NotFound,
    InvalidInput,
    ParseError,
    IoError,
    Internal,
    /// Operation exceeded its time budget (status --wait, eval timeout, etc).
    Timeout,
}

// src/cli/json_envelope.rs:96-106 — add the as_str arm
impl ErrorCode {
    pub const fn as_str(&self) -> &'static str {
        match self {
            ErrorCode::NotFound => "not_found",
            ErrorCode::InvalidInput => "invalid_input",
            ErrorCode::ParseError => "parse_error",
            ErrorCode::IoError => "io_error",
            ErrorCode::Internal => "internal",
            ErrorCode::Timeout => "timeout",
        }
    }
}

// src/cli/json_envelope.rs:125-139 — add the constant
pub mod error_codes {
    use super::ErrorCode;
    pub const NOT_FOUND: &str = ErrorCode::NotFound.as_str();
    pub const INVALID_INPUT: &str = ErrorCode::InvalidInput.as_str();
    pub const PARSE_ERROR: &str = ErrorCode::ParseError.as_str();
    pub const IO_ERROR: &str = ErrorCode::IoError.as_str();
    pub const INTERNAL: &str = ErrorCode::Internal.as_str();
    /// Operation exceeded its time budget. Used by `cqs status --wait`
    /// timeout and any future time-bounded operation that times out
    /// before producing a result.
    pub const TIMEOUT: &str = ErrorCode::Timeout.as_str();
}
```

The `ErrorCode` enum is `#[non_exhaustive]` so adding a variant is non-breaking.

**Step 2.** `emit_json_error_with_data` does not exist (verified — only `emit_json_error` exists at `src/cli/json_envelope.rs:352`). Add it as a thin variant beside `emit_json_error`:

```rust
// src/cli/json_envelope.rs — new helper next to emit_json_error
/// Like `emit_json_error` but carries an optional `data` payload alongside
/// the error so consumers can still surface counters (snapshot, wait_secs,
/// etc). Used by `cqs status --wait` timeout to embed the stale snapshot
/// in the error envelope.
pub fn emit_json_error_with_data(
    code: &str,
    message: &str,
    data: Option<serde_json::Value>,
) -> Result<()> {
    let mut env = serde_json::Map::with_capacity(4);
    env.insert(
        "data".to_string(),
        data.unwrap_or(serde_json::Value::Null),
    );
    env.insert(
        "error".to_string(),
        serde_json::json!({"code": code, "message": message}),
    );
    env.insert(
        "version".to_string(),
        serde_json::Value::Number(JSON_OUTPUT_VERSION.into()),
    );
    env.insert("_meta".to_string(), serde_json::to_value(EnvelopeMeta::new())?);
    let buf = serde_json::Value::Object(env);
    let s = format_envelope_to_string(&buf)?;
    println!("{s}");
    Ok(())
}
```

**Step 3.** Update the timeout arm in `status.rs`:

```rust
            cqs::daemon_translate::FreshnessWait::Timeout(snap) => {
                if json {
                    // API-V1.30.1-1: error envelope so JSON consumers
                    // see error.code="timeout" alongside the non-zero exit
                    // code. Embed the snapshot in the error data so callers
                    // can still surface counters.
                    let payload = serde_json::json!({
                        "snapshot": snap,
                        "wait_secs": budget_secs,
                    });
                    crate::cli::json_envelope::emit_json_error_with_data(
                        crate::cli::json_envelope::error_codes::TIMEOUT,
                        &format!("watch index still stale after {budget_secs}s"),
                        Some(payload),
                    )?;
                } else {
                    print_text(&snap);
                    eprintln!(
                        "cqs: watch index still stale after {budget_secs}s wait",
                    );
                }
                std::process::exit(1);
            }
```

### Verification

- `cargo build --features gpu-index`.
- `cargo test --features gpu-index --lib json_envelope` (covers new variant + helper).
- Manual: stop daemon, queue files, run `cqs status --watch-fresh --wait --wait-secs 1 --json`; check stdout JSON has `"error":{"code":"timeout",...}` and exit code is 1.
- Update / add `tests/cli_status_test.rs` to pin the envelope shape with the new `code: "timeout"`.

---

## P2: API-V1.30.1-5 — `daemon_ping`/`status`/`reconcile` return `Result<T, String>` (folded into P2-bundle-wait-fresh)

**Files:** `src/daemon_translate.rs:271, 422, 541`
**Effort:** subsumed by P2-bundle-wait-fresh's `DaemonStatusError` enum
**Why:** Stringly-typed errors on the public API. Three call sites with overlapping failure modes (socket-missing, transport, parse) collapse into opaque strings. Caller can't distinguish "daemon never ran" from "daemon crashed mid-call" from "daemon answered with garbage".

### Approach

The wait-fresh bundle already introduces `DaemonStatusError { SocketMissing, Transport, BadResponse }`. Apply the same enum to all three RPCs:

```rust
#[cfg(unix)]
#[derive(Debug, Clone, thiserror::Error)]
pub enum DaemonRpcError {
    #[error("daemon socket missing: {0}")]
    SocketMissing(String),
    #[error("daemon transport failure: {0}")]
    Transport(String),
    #[error("daemon returned malformed response: {0}")]
    BadResponse(String),
    #[error("daemon error: {0}")]
    DaemonError(String),
}

pub fn daemon_ping(cqs_dir: &std::path::Path) -> Result<PingResponse, DaemonRpcError> { ... }
pub fn daemon_status(cqs_dir: &std::path::Path) -> Result<WatchSnapshot, DaemonRpcError> { ... }
pub fn daemon_reconcile(...) -> Result<DaemonReconcileResponse, DaemonRpcError> { ... }
```

Map existing `format!(...)` errors to the appropriate variant:
- `if !sock_path.exists()` → `SocketMissing`
- `UnixStream::connect`, `set_*_timeout`, `write`, `read` failures → `Transport`
- `serde_json::from_str` envelope parse, missing/non-string `status` field, deserialize → `BadResponse`
- Daemon-returned `status: "err"` envelopes → `DaemonError`

### Verification

- `cargo build --features gpu-index` — likely many call-site fixups (eval/mod.rs, status.rs, hook.rs, doctor.rs); expect ~20-30 lines of churn across callers.
- All existing tests should pass after updating `Err(String)` to `Err(DaemonRpcError::Variant(_))`.

---

## P2: API-V1.30.1-10 — `WatchSnapshot.idle_secs` frozen at compute time — wire shape lies once snapshot served later

**Files:** `src/watch_status.rs:101, 219`
**Effort:** ~45 minutes
**Why:** `idle_secs` is computed via `last_event.elapsed().as_secs()` at snapshot-publish time, but the snapshot can be read by clients seconds later (the daemon publishes every ~100 ms but clients poll arbitrarily). The wire shape claims "seconds since last event" — but it's actually "seconds since last event as of N seconds ago." Consumers gating on `idle_secs > threshold` get a stale answer.

### Current code

```rust
// src/watch_status.rs:101
    pub idle_secs: u64,

// src/watch_status.rs:219
            idle_secs: input.last_event.elapsed().as_secs(),
```

### Replacement / approach

Two paths — pick one:

**(a) Compute idle on read.** Change `idle_secs` to be derived at JSON-serialization time from a `last_event_unix_secs: i64` field stored in the snapshot. Requires custom serde or a `to_wire` helper. More invasive.

**(b) Document and rename.** Rename the field on the wire to `idle_secs_at_snapshot` and add `snapshot_at` (already exists at line 109) — consumers compute `now - last_event_unix_secs` on their side. Add `last_event_unix_secs: i64` (Unix seconds when the event happened) to the wire shape. Keep `idle_secs` for backcompat as `snapshot_at - last_event_unix_secs` so existing JSON consumers don't break, but mark deprecated in the doc.

**Recommended: (b)** because it makes the wire shape self-describing and consumers can compute fresher idle on demand:

```rust
// src/watch_status.rs:75-110 — add new field
pub struct WatchSnapshot {
    // ... existing fields ...
    /// Unix timestamp (seconds) of the last filesystem event the watch
    /// loop observed. Lets clients compute fresher idle on demand
    /// without retransacting through the daemon. Pair with `snapshot_at`
    /// to compute `idle_at_snapshot_time` if you need historical value.
    pub last_event_unix_secs: i64,
    // idle_secs becomes derived but kept for backcompat:
    /// Snapshot-time idle seconds. For fresh idle, prefer
    /// `now - last_event_unix_secs`.
    pub idle_secs: u64,
    // ... existing snapshot_at ...
}
```

Plumb `last_event_unix_secs` through `WatchSnapshotInput` (compute once at publish: `last_event_unix_secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64 - last_event.elapsed().as_secs() as i64).unwrap_or(0)`).

### Verification

- `cargo build --features gpu-index`.
- `cargo test --features gpu-index --lib watch_status`.
- Manual: `cqs status --watch-fresh --json`, sleep 5 s, run again — `last_event_unix_secs` is the same value across both calls if no events fired; `idle_secs` differs by 5.

---

## P2: OB-V1.30.1-9 — `process_file_changes` uses `println!` in non-quiet mode

**Files:** `src/cli/watch/events.rs:147-152`
**Effort:** ~15 minutes
**Why:** Daemon process writes user-facing UI to stdout — bypasses tracing infrastructure, can't be filtered by log level, breaks structured-log parsers.

### Current code

```rust
// src/cli/watch/events.rs:147-152
    if !cfg.quiet {
        println!("\n{} file(s) changed, reindexing...", files.len());
        for f in &files {
            println!("  {}", f.display());
        }
    }
```

### Replacement / approach

```rust
    // OB-V1.30.1-9: replace stdout println with structured tracing.
    // The daemon has no terminal — stdout goes to journald via the
    // systemd unit which writes unstructured. Tracing routes through
    // the configured subscriber (journald JSON or stderr text) and
    // honours filter levels.
    tracing::info!(
        file_count = files.len(),
        files = ?files,
        "watch: reindexing changed files",
    );
```

If a foreground (non-daemon) UX really needs the unstructured print, gate it on a separate `cfg.foreground` or new `cfg.show_progress` flag, not `!cfg.quiet`. The daemon is the sole runtime caller today.

### Verification

- `cargo build --features gpu-index`.
- Restart daemon, edit a file, `journalctl --user -u cqs-watch -o json | jq '.MESSAGE' | head -3` shows the structured event.

---

## P2: OB-V1.30.1-10 — `serve::search` info logs full query at info — bypasses TraceLayer redaction

**Files:** `src/serve/handlers.rs:189-232`
**Effort:** ~15 minutes
**Why:** `tracing::info!(query = %params.q, ...)` at line 193 logs the user's full search query at info level. TraceLayer already records the URI with redaction; this duplicate log bypasses it. A user accidentally pasting a credential as a search query writes the credential to journal.

### Current code

```rust
// src/serve/handlers.rs:189-232
pub(crate) async fn search(
    State(state): State<AppState>,
    Query(params): Query<SearchQuery>,
) -> Result<Json<SearchResponse>, ServeError> {
    tracing::info!(query = %params.q, limit = params.limit, "serve::search");

    // ... rest unchanged ...

    tracing::info!(matches = matches.len(), "search returned");
    Ok(Json(SearchResponse { matches }))
}
```

### Replacement / approach

```rust
pub(crate) async fn search(
    State(state): State<AppState>,
    Query(params): Query<SearchQuery>,
) -> Result<Json<SearchResponse>, ServeError> {
    // OB-V1.30.1-10: log only metadata at info; full query at debug
    // so it's available for local debugging but not journal-retained
    // by default. The TraceLayer span already has the redacted URI.
    tracing::debug!(query = %params.q, "serve::search query received");
    tracing::info!(q_len = params.q.len(), limit = params.limit, "serve::search");

    // ... rest unchanged ...

    tracing::info!(matches = matches.len(), "search returned");
    Ok(Json(SearchResponse { matches }))
}
```

### Verification

- `cargo build --features gpu-index`.
- Run `cqs serve` with default `RUST_LOG`, send a search request via curl, confirm `q=...` does not appear in journal at info; appears only at debug.

---

## P2: PB-V1.30.1-1 — `cmd_serve` `--no-auth` warning misses `0.0.0.0` and `::` wildcard binds

**Files:** `src/cli/commands/serve.rs:27`
**Effort:** ~20 minutes
**Why:** The "non-loopback + no-auth" warning fires on `--bind 192.168.1.5 --no-auth` but is silent for `0.0.0.0` and `::` — the *most* exposed bind targets. The current substring check `bind != "127.0.0.1" && bind != "localhost" && bind != "::1"` passes wildcard strings unchanged.

### Current code

```rust
// src/cli/commands/serve.rs:27
    if no_auth && bind != "127.0.0.1" && bind != "localhost" && bind != "::1" {
        tracing::warn!(
            bind = %bind,
            "binding cqs serve to non-localhost without auth — anyone with network \
             access to this address can read the index"
        );
        eprintln!(
            "WARN: --bind {bind} with --no-auth exposes cqs serve beyond localhost \
             with no authentication"
        );
    }
```

### Replacement / approach

```rust
    if no_auth {
        // PB-V1.30.1-1: parse `bind` once and warn on anything that
        // doesn't resolve to a loopback address. This subsumes 0.0.0.0
        // and :: (UNSPECIFIED — most exposed configs of all), concrete
        // LAN IPs, and hostnames that don't loop back. Parse-failure
        // (e.g. "localhost") falls through to the explicit name check.
        let is_loopback = match bind.parse::<std::net::IpAddr>() {
            Ok(ip) => ip.is_loopback(),
            Err(_) => matches!(bind.as_str(), "localhost"),
        };
        if !is_loopback {
            tracing::warn!(
                bind = %bind,
                "binding cqs serve to non-localhost without auth — anyone with network \
                 access to this address can read the index"
            );
            eprintln!(
                "WARN: --bind {bind} with --no-auth exposes cqs serve beyond localhost \
                 with no authentication"
            );
        }
    }
```

### Verification

- `cargo build --features gpu-index`.
- `cqs serve --no-auth --bind 0.0.0.0` emits the warn line.
- `cqs serve --no-auth --bind ::` emits the warn line.
- `cqs serve --no-auth --bind 127.0.0.1` does not.
- Add a small unit test if a `serve_warn_decision(bind: &str, no_auth: bool) -> bool` helper is factored out.

---

## P2: PB-V1.30.1-3 — `process_exists` (Windows) substring-matches localized `tasklist` output

**Files:** `src/cli/files.rs:59-72`
**Effort:** ~45 minutes
**Why:** `tasklist /FI "PID eq <pid>" /NH` emits `INFO:` only on English Windows. German `INFORMATION:`, French `INFORMATIONS:`, Japanese `情報:`, etc. silently bypass the stale-PID detection, causing every non-English Windows user to see persistent stale-lock errors.

### Current code

```rust
// src/cli/files.rs:59-72
#[cfg(windows)]
fn process_exists(pid: u32) -> bool {
    use std::process::Command;
    Command::new("tasklist")
        .args(["/FI", &format!("PID eq {}", pid), "/NH"])
        .output()
        .map(|o| {
            let output = String::from_utf8_lossy(&o.stdout);
            // tasklist /FI "PID eq N" does exact filtering.
            // "INFO:" appears when no process matches; its absence means a match.
            !output.contains("INFO:")
        })
        .unwrap_or(false)
}
```

### Replacement / approach

```rust
#[cfg(windows)]
fn process_exists(pid: u32) -> bool {
    use std::process::Command;
    // PB-V1.30.1-3: CSV format is locale-independent. tasklist /NH /FO CSV
    // emits exactly one row per match; empty stdout (or whitespace only)
    // means no match. No human-readable strings to misinterpret.
    Command::new("tasklist")
        .args(["/FI", &format!("PID eq {}", pid), "/NH", "/FO", "CSV"])
        .output()
        .map(|o| {
            let output = String::from_utf8_lossy(&o.stdout);
            // A successful match looks like: "cqs.exe","1234","Console",... CRLF
            // No match → empty or whitespace-only output.
            output.trim().contains(&format!(",\"{}\",", pid))
        })
        .unwrap_or(false)
}
```

The `.contains(&format!(",\"{},\"", pid))` check matches the CSV `pid` column to defend against substring collisions (e.g., PID `12` matching PID `1234`).

### Verification

- `cargo build --target x86_64-pc-windows-msvc --features gpu-index` (or run on Windows).
- Add a unit test: parse a sample CSV output blob with and without the target PID column.
- Smoke on a non-English Windows VM: kill an old PID, run `cqs index`, confirm the stale-lock-retry loop fires instead of the immediate fail.

---

## P2: PB-V1.30.1-7 — `cqs hook fire` on Windows-native: `.cqs/.dirty` written but no consumer reads it

**Files:** `src/cli/commands/infra/hook.rs:309-335`, `src/cli/commands/index/build.rs` (consumer side)
**Effort:** ~45 minutes
**Why:** On Windows-native, `cqs hook fire` falls through to `.cqs/.dirty` because the daemon path is `#[cfg(unix)]`. But the `.cqs/.dirty` consumer at `watch/mod.rs:594` is *also* `#[cfg(unix)]`. Net: Windows-native users get a marker nothing reads. They must run `cqs index` manually after every git op.

### Current code

```rust
// src/cli/commands/infra/hook.rs:323-332
    #[cfg(not(unix))]
    {
        report.daemon_error = Some("hook fire requires unix sockets".to_string());
    }

    // Fallback: leave a marker the daemon will pick up on next start.
    let dirty = cqs_dir.join(".dirty");
    std::fs::create_dir_all(&cqs_dir).with_context(|| format!("create {}", cqs_dir.display()))?;
    std::fs::write(&dirty, b"").with_context(|| format!("touch {}", dirty.display()))?;
    report.dirty_marker = Some(dirty);
```

### Replacement / approach

Make `cqs index` (the foreground reindex command) check for `.cqs/.dirty` at startup and consume it. This gives Windows users equivalent functionality on next manual reindex.

1. Add a helper in `src/cli/commands/index/build.rs` (or a new `dirty_marker` module):

```rust
/// Check `.cqs/.dirty` and consume it (delete) at startup.
///
/// Daemon-less platforms (Windows-native) write this marker via
/// `cqs hook fire`; the next `cqs index` clears it as evidence
/// that the requested reindex has occurred.
pub(crate) fn consume_dirty_marker(cqs_dir: &Path) -> bool {
    let marker = cqs_dir.join(".dirty");
    if marker.exists() {
        if let Err(e) = std::fs::remove_file(&marker) {
            tracing::warn!(error = %e, "failed to remove .dirty marker");
        }
        true
    } else {
        false
    }
}
```

2. Call it at the top of `cmd_index` (`src/cli/commands/index/build.rs`):

```rust
let dirty_consumed = consume_dirty_marker(&cqs_dir);
if dirty_consumed {
    tracing::info!("consumed .cqs/.dirty marker — reindex triggered by hook");
}
```

3. Update the `cqs hook install` Windows-native warning so users understand the `cqs index` requirement:

```rust
#[cfg(windows)]
fn cmd_install(...) {
    eprintln!(
        "Note: on Windows-native, hooks write `.cqs/.dirty` and your next \
         `cqs index` will pick it up. Run `cqs index` after major git ops."
    );
}
```

### Verification

- `cargo build --features gpu-index`.
- Manual on a Windows VM: install hook, run a git op, confirm `.cqs/.dirty` appears, then `cqs index` removes it and reindexes.

---

## P2: SEC-V1.30.1-3 — `callgraph-3d.js` interpolates `e.message` into innerHTML without escapeHtml

**Files:** `src/serve/assets/views/callgraph-3d.js:55`
**Effort:** ~15 minutes
**Why:** SEC-2 hardening landed `escapeHtml` mirrors in cluster-3d.js and hierarchy-3d.js with explicit comments. callgraph-3d.js missed the pass. Defence-in-depth XSS gap on the bundle-load-failure error path.

### Current code

```js
// src/serve/assets/views/callgraph-3d.js:43-58
    async init(container, options) {
      this.container = container;
      this.cb = options.callbacks || {};
      container.innerHTML =
        '<div style="margin:24px;color:#666">loading 3D renderer…</div>';

      if (typeof window.cqsEnsureThreeBundle === "function") {
        try {
          await window.cqsEnsureThreeBundle();
        } catch (e) {
          container.innerHTML = `<div class="error" style="margin:24px">3D bundle failed to load: ${e.message}</div>`;
          throw e;
        }
      }
```

### Replacement / approach

Mirror the `escapeHtml` helper at the top of the IIFE (matching `cluster-3d.js:21` / `hierarchy-3d.js:19`) and use it on the interpolation:

```js
// At top of the IIFE, alongside other helpers:
// SEC-V1.30.1-3 / SEC-2 mirror: this IIFE can't reach app.js's escapeHtml,
// so mirror it here for any server-derived string interpolated into innerHTML.
const escapeHtml = (s) =>
  String(s).replace(/[&<>"']/g, (c) => ({
    "&": "&amp;",
    "<": "&lt;",
    ">": "&gt;",
    '"': "&quot;",
    "'": "&#39;",
  }[c]));

// At line 55:
container.innerHTML = `<div class="error" style="margin:24px">3D bundle failed to load: ${escapeHtml(e.message)}</div>`;
```

### Verification

- `cargo build --features gpu-index` (no Rust changes; ensures asset reloads).
- Manual: simulate a bundle load failure (point the script tag at a 404), confirm error renders as text not HTML.

---

## P2: SEC-V1.30.1-4 — `tag_user_code_trust_level` is shape-coupled

**Files:** `src/cli/commands/mod.rs:216-257`
**Effort:** ~60 minutes
**Why:** Walks four hardcoded JSON shapes (`entry_point`, `call_chain`, `callers`, `file_groups[].chunks[]`). Any chunk-shaped object outside these (a future `dependents[]`, `examples[]`, top-level `chunks[]`) is silently emitted with no `trust_level` field. The contract claims "every chunk-returning JSON output carries `trust_level`" but the implementation is "every chunk in one of these four arrays."

### Current code

```rust
// src/cli/commands/mod.rs:216-257
pub(crate) fn tag_user_code_trust_level(json: &mut serde_json::Value) {
    fn tag(obj: &mut serde_json::Map<String, serde_json::Value>) {
        obj.insert(
            "trust_level".to_string(),
            serde_json::Value::String("user-code".to_string()),
        );
    }
    if let Some(root) = json.as_object_mut() {
        if let Some(ep) = root.get_mut("entry_point").and_then(|v| v.as_object_mut()) {
            tag(ep);
        }
        // ... three more hand-rolled walks ...
    }
}
```

### Replacement / approach

Replace the four-shape walker with a recursive visitor that detects chunk-shape signatures:

```rust
pub(crate) fn tag_user_code_trust_level(json: &mut serde_json::Value) {
    // SEC-V1.30.1-4: recursive visitor — any object with the chunk-shape
    // signature (presence of `name` AND `file` AND a numeric `line_start`)
    // gets tagged. Future scout/onboard surfaces that grow new
    // chunk-bearing keys are tagged automatically.
    fn looks_like_chunk(obj: &serde_json::Map<String, serde_json::Value>) -> bool {
        obj.contains_key("name")
            && obj.contains_key("file")
            && obj.get("line_start").is_some_and(|v| v.is_number())
    }

    fn walk(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Object(map) => {
                if looks_like_chunk(map) {
                    map.insert(
                        "trust_level".to_string(),
                        serde_json::Value::String("user-code".to_string()),
                    );
                }
                for (_k, v) in map.iter_mut() {
                    walk(v);
                }
            }
            serde_json::Value::Array(arr) => {
                for v in arr.iter_mut() {
                    walk(v);
                }
            }
            _ => {}
        }
    }

    walk(json);
}
```

### Tests to add

```rust
#[test]
fn tag_user_code_visits_arbitrary_nested_chunks() {
    let mut json = serde_json::json!({
        "entry_point": {"name": "foo", "file": "a.rs", "line_start": 10},
        "future_field": {
            "examples": [
                {"name": "bar", "file": "b.rs", "line_start": 20},
            ]
        }
    });
    tag_user_code_trust_level(&mut json);
    assert_eq!(json["entry_point"]["trust_level"], "user-code");
    assert_eq!(json["future_field"]["examples"][0]["trust_level"], "user-code");
}

#[test]
fn tag_user_code_does_not_tag_non_chunk_objects() {
    let mut json = serde_json::json!({"meta": {"version": 1}});
    tag_user_code_trust_level(&mut json);
    assert!(json["meta"].get("trust_level").is_none());
}
```

### Verification

- `cargo test --features gpu-index --lib commands -- tag_user_code`.
- Confirm scout / onboard / where output JSON shapes still pass downstream consumers.

---

## P2: DS-V1.30.1-D1 — `cqs index --force` reopen leaves stale `pending_rebuild`

**Files:** `src/cli/watch/mod.rs:1102-1122`
**Effort:** ~45 minutes
**Why:** When the watch loop detects `cqs index --force` rotated `index.db`, it reopens the Store but does NOT reset `state.pending_rebuild`. The in-flight rebuild's pre-rotation delta references OLD DB chunk IDs; replaying it against the NEW HNSW corrupts vectors.

### Current code

```rust
// src/cli/watch/mod.rs:1102-1122
                    let current_id = db_file_identity(&index_path);
                    if current_id != db_id {
                        info!("index.db replaced (likely cqs index --force), reopening store");
                        drop(store);
                        store = Store::open_with_runtime(&index_path, Arc::clone(&shared_rt))
                            .with_context(|| {
                                format!(
                                    "Failed to re-open store at {} after DB replacement",
                                    index_path.display()
                                )
                            })?;
                        state.hnsw_index = None;
                        state.incremental_count = 0;
                    }
```

### Replacement / approach

```rust
                    let current_id = db_file_identity(&index_path);
                    if current_id != db_id {
                        info!("index.db replaced (likely cqs index --force), reopening store");
                        drop(store);
                        store = Store::open_with_runtime(&index_path, Arc::clone(&shared_rt))
                            .with_context(|| {
                                format!(
                                    "Failed to re-open store at {} after DB replacement",
                                    index_path.display()
                                )
                            })?;
                        state.hnsw_index = None;
                        state.incremental_count = 0;
                        // DS-V1.30.1-D1: drop in-flight rebuild whose pending
                        // delta references OLD DB chunk IDs. The rebuild
                        // thread will tx.send(...) into a dropped receiver
                        // (no-op per rebuild.rs:289). Force a fresh rebuild
                        // on the next threshold tick against the new DB.
                        if state.pending_rebuild.take().is_some() {
                            tracing::info!(
                                "discarded in-flight HNSW rebuild after DB replacement; \
                                 next threshold tick will rebuild against new DB",
                            );
                        }
                    }
```

### Verification

- `cargo build --features gpu-index`.
- Test: simulate concurrent `cqs index --force` while a rebuild is in flight, confirm the pending_rebuild is dropped and the next snapshot doesn't show stale `delta_saturated`.

---

## P2: DS-V1.30.1-D2 — `run_daemon_reconcile` bypasses `max_pending_files()` cap

**Files:** `src/cli/watch/reconcile.rs:103, 128`, `src/cli/watch/events.rs` (max_pending_files reference)
**Effort:** ~45 minutes
**Why:** Reconcile blindly inserts every divergent file into `pending_files` with no cap check. A `git checkout` of a sibling branch with 50k file changes pushes the queue well past `max_pending_files()` (default 5000), defeating the documented backpressure. Subsequent inotify events increment `dropped_this_cycle` for unrelated reasons; the next `process_file_changes` tries to drain 50k files in one batch.

### Current code

```rust
// src/cli/watch/reconcile.rs:95-134
    for rel in disk_files {
        let origin = rel.to_string_lossy().replace('\\', "/");
        match indexed.get(&origin) {
            None => {
                if pending_files.insert(rel.clone()) {
                    added += 1;
                    queued += 1;
                }
            }
            Some(stored_mtime) => {
                // ...
                if needs_reindex && pending_files.insert(rel.clone()) {
                    modified += 1;
                    queued += 1;
                }
            }
        }
    }
```

### Replacement / approach

Pass `max_pending_files()` into reconcile and stop inserting once the queue hits the cap. Track skipped count locally:

```rust
pub(super) fn run_daemon_reconcile(
    store: &Store,
    root: &Path,
    parser: &CqParser,
    no_ignore: bool,
    pending_files: &mut HashSet<PathBuf>,
    cap: usize,  // new parameter — caller passes max_pending_files()
) -> usize {
    let _span = tracing::info_span!("daemon_reconcile").entered();
    // ... existing setup ...

    let mut added = 0usize;
    let mut modified = 0usize;
    let mut queued = 0usize;
    let mut skipped_cap = 0usize;
    for rel in disk_files {
        // DS-V1.30.1-D2: respect the same backpressure cap as the inotify
        // ingest path (events.rs:108). Without this, a bulk branch switch
        // can push the queue to 50k files on one tick, masking subsequent
        // genuine drop signals and forcing a 50k-file synchronous drain.
        if pending_files.len() >= cap {
            skipped_cap += 1;
            continue;
        }

        // ... existing match dispatch using pending_files.insert ...
    }

    if skipped_cap > 0 {
        tracing::warn!(
            cap,
            skipped = skipped_cap,
            "Reconcile: queue cap reached; deferred {skipped_cap} divergent files \
             to next reconcile pass",
        );
    }
    // ... existing terminal info/debug ...
    queued
}
```

Update **all 7 call sites** when adding the `cap: usize` parameter — the signature change cascades:

**Production (2 sites):**
- `src/cli/watch/mod.rs:1055` — pass `crate::cli::watch::events::max_pending_files()`
- `src/cli/watch/mod.rs:1268` — same

**Tests (5 sites in the same file as the function):**
- `src/cli/watch/reconcile.rs:190`
- `src/cli/watch/reconcile.rs:204`
- `src/cli/watch/reconcile.rs:225`
- `src/cli/watch/reconcile.rs:362`
- `src/cli/watch/reconcile.rs:450`

Tests can pass any value — `usize::MAX` to keep behaviour unchanged for tests not exercising the cap, or a small integer (10) for the new `reconcile_respects_pending_cap` test.

### Verification

- `cargo test --features gpu-index --lib reconcile` (all 5 existing tests must still pass after signature update).
- Add `reconcile_respects_pending_cap` test: seed 100 divergent files, set `cap=10`, run reconcile, assert `pending_files.len() == 10` and the warn log fired with `skipped=90`.
- Compile-time check via the cascade: removing or renaming a call site argument fails the build immediately, so the 5-test sweep is mechanical.

---

## P2: DS-V1.30.1-D5 — `.cqs/.dirty` fallback marker write not atomic

**Reframed during verification:** original prompt called `atomic_replace(&dirty, b"")` with wrong arity; real signature is `(tmp_path: &Path, final_path: &Path) -> io::Result<()>`. Replacement now stages bytes to a `.dirty.tmp` sibling, then promotes via `atomic_replace`.

**Files:** `src/cli/commands/infra/hook.rs:329-332`
**Effort:** ~15 minutes
**Why:** `std::fs::write(&dirty, b"")` is a non-atomic open+write+close, no fsync. On crash + power loss between the hook's write and the next fs sync of `.cqs/`, the file's directory entry can be lost. Since `cqs hook` exists *specifically* for the daemon-offline case, losing the marker means a `git checkout` post-reboot won't trigger reconcile.

### Current code

```rust
// src/cli/commands/infra/hook.rs:329-332
    let dirty = cqs_dir.join(".dirty");
    std::fs::create_dir_all(&cqs_dir).with_context(|| format!("create {}", cqs_dir.display()))?;
    std::fs::write(&dirty, b"").with_context(|| format!("touch {}", dirty.display()))?;
```

### Replacement / approach

`cqs::fs::atomic_replace` (real signature `(tmp_path: &Path, final_path: &Path) -> io::Result<()>` per `src/fs.rs:41`) is a write-tmp-then-rename helper, not a write-bytes helper. For the empty marker, write `b""` to a `.dirty.tmp` sibling first, then promote:

```rust
    let dirty = cqs_dir.join(".dirty");
    std::fs::create_dir_all(&cqs_dir).with_context(|| format!("create {}", cqs_dir.display()))?;
    // DS-V1.30.1-D5: stage to .dirty.tmp then atomic_replace so the
    // marker survives a power-cut between write and the next directory
    // sync. atomic_replace fsyncs the tmp before rename and best-effort
    // fsyncs the parent afterwards. The marker is the *only* signal the
    // daemon will see post-reboot, so durability matters more than the
    // empty-file write cost.
    let tmp = cqs_dir.join(".dirty.tmp");
    std::fs::write(&tmp, b"").with_context(|| format!("stage {}", tmp.display()))?;
    cqs::fs::atomic_replace(&tmp, &dirty)
        .with_context(|| format!("promote {} -> {}", tmp.display(), dirty.display()))?;
```

`crate::fs::atomic_replace` already exists (used by `notes.toml`, `audit-mode.json`, slot writers post-#P3.39). It owns the fsync-tmp-then-rename-then-fsync-parent sequence — caller stages the bytes.

### Verification

- `cargo build --features gpu-index`.
- `cargo test --features gpu-index --lib hook`.
- Add a test that writes the marker, confirms it exists, the file size is 0, and `.dirty.tmp` was cleaned up by the rename.

---

## P2: DS-V1.30.1-D7 — HNSW rollback path leaves `.bak` files orphaned when restore-rename fails

**Reframed during verification:** original prompt mis-named the function as `save_owned` (real name: `save` per `src/hnsw/persist.rs:208`) and used `anyhow::anyhow!` / `anyhow::bail!` inside a function returning `Result<(), HnswError>`. Replacement now uses the correct name and returns `HnswError::Internal(...)` so the patch compiles.

**Files:** `src/hnsw/persist.rs:509-553`
**Effort:** ~45 minutes
**Why:** Rollback path iterates `all_exts` and on `std::fs::rename` failure logs an error and continues — so `bak_path` is in an indeterminate state. The next `save` skips the rename-back step (line 436 guards on `final_path.exists()`), then writes new finals, and a subsequent rollback restores from the *stale* `.bak` from the prior failure. Silently overwrites known-good index with known-bad backup.

### Current code

```rust
// src/hnsw/persist.rs:509-553 (inside `pub fn save(&self, dir: &Path, basename: &str) -> Result<(), HnswError>`)
        if let Err(e) = rename_result {
            // Roll back: remove new files and restore originals from .bak
            for ext in &moved_exts {
                let final_path = dir.join(format!("{}.{}", basename, ext));
                let _ = std::fs::remove_file(&final_path);
            }
            for ext in &all_exts {
                let bak_path = dir.join(format!("{}.{}.bak", basename, ext));
                let final_path = dir.join(format!("{}.{}", basename, ext));
                if bak_path.exists() {
                    if let Err(e) = std::fs::rename(&bak_path, &final_path) {
                        tracing::error!(
                            path = %final_path.display(),
                            error = %e,
                            "Failed to restore backup during HNSW save rollback"
                        );
                    }
                }
            }
            // ... fsync + final cleanup ...
        }
```

### Replacement / approach

Track which `.bak` files were successfully restored. On any failure, leave the un-restored ones in place AND emit a clear recovery breadcrumb. At the start of `save`, refuse the new save if `.bak` files exist (evidence of a prior incomplete rollback).

`save` returns `Result<(), HnswError>`, so error returns must produce `HnswError` variants (use `HnswError::Internal(String)` — see `src/hnsw/mod.rs:115`). Do **not** introduce `anyhow` here.

```rust
        if let Err(e) = rename_result {
            for ext in &moved_exts {
                let final_path = dir.join(format!("{}.{}", basename, ext));
                let _ = std::fs::remove_file(&final_path);
            }
            // DS-V1.30.1-D7: track which .bak files were successfully
            // restored. Any unrestored ones are left in place as recovery
            // breadcrumbs and the operator gets an actionable error.
            let mut restore_failures: Vec<(String, std::io::Error)> = Vec::new();
            for ext in &all_exts {
                let bak_path = dir.join(format!("{}.{}.bak", basename, ext));
                let final_path = dir.join(format!("{}.{}", basename, ext));
                if bak_path.exists() {
                    if let Err(rename_err) = std::fs::rename(&bak_path, &final_path) {
                        restore_failures.push((final_path.display().to_string(), rename_err));
                    }
                }
            }

            if !restore_failures.is_empty() {
                let detail: String = restore_failures
                    .iter()
                    .map(|(p, e)| format!("  {p}: {e}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                tracing::error!(
                    %detail,
                    dir = %dir.display(),
                    "HNSW rollback INCOMPLETE — manual recovery required: \
                     restore .bak files matching {basename}.*.bak in dir",
                );
                // Don't fsync over a half-restored state; surface the
                // partial failure so the next save can refuse cleanly.
                let _ = std::fs::remove_dir_all(&temp_dir);
                return Err(HnswError::Internal(format!(
                    "HNSW rollback partially failed; .bak files present, \
                     manual recovery required:\n{detail}"
                )));
            }
            // ... existing fsync logic for the all-restored case ...
        }
```

Add a guard at the start of `save` (right after the count-mismatch check at line 214):

```rust
pub fn save(&self, dir: &Path, basename: &str) -> Result<(), HnswError> {
    // ... existing _span + count-mismatch check ...

    // DS-V1.30.1-D7: refuse to start a new save if a previous rollback
    // left .bak files behind. The operator must clear them manually so
    // we don't silently overwrite a known-good index with a stale .bak
    // on a future rollback.
    let all_exts = ["hnsw.graph", "hnsw.data", "hnsw.ids", "hnsw.checksum"];
    let stale_baks: Vec<std::path::PathBuf> = all_exts
        .iter()
        .filter_map(|ext| {
            let bak = dir.join(format!("{}.{}.bak", basename, ext));
            if bak.exists() { Some(bak) } else { None }
        })
        .collect();
    if !stale_baks.is_empty() {
        return Err(HnswError::Internal(format!(
            "stale .bak files from prior failed save: {:?}; manual recovery required \
             (remove them or rename to current files)",
            stale_baks,
        )));
    }
    // ... existing body that creates target dir, locks, etc. ...
}
```

(Note: `all_exts` is also defined locally lower down at the existing line ~421; either hoist it once or scope this guard's copy with a different name. Hoisting is cleaner.)

### Verification

- `cargo build --features gpu-index`.
- `cargo test --features gpu-index --lib hnsw -- save_rollback`.
- Add a test that injects a rename failure mid-rollback and asserts the operator-actionable error message (`HnswError::Internal` with the breadcrumb text) + that subsequent `save` calls bail with the stale-bak guard.

---

## P2: TC-HAP-1.30.1-2 — `cmd_uninstall`, `cmd_fire`, `cmd_status` (hook status) ship with zero tests

**Reframed during verification:** original title and test names referenced `cmd_hook_status`, but the real function is `cmd_status` (verified at `src/cli/commands/infra/hook.rs:339`). Renamed test + extracted helper. Also clarified the marker constants: code matches with `HOOK_MARKER_PREFIX` (`"# cqs:hook"` per line 45), and `HOOK_MARKER_CURRENT` (`"# cqs:hook v1"` per line 46) is what install writes — `CURRENT` contains `PREFIX` so seeding tests with `HOOK_MARKER_CURRENT` exercises the same matcher.

**Files:** `src/cli/commands/infra/hook.rs:262-373`, plus new `tests/cli_hook_test.rs` or in-module tests
**Effort:** ~75 minutes
**Why:** Three #1182 CLI commands ship with no test coverage. `cmd_uninstall` foreign-hook-skip branch, `cmd_fire` daemon-fallback branch, and `cmd_status` three-state classifier (installed / foreign / missing) are all unverified.

### Approach

Add three tests in `src/cli/commands/infra/hook.rs::tests` (the file already has a `tests` module per finding TC-HAP-1.30.1-1):

```rust
#[test]
fn cmd_uninstall_removes_only_marked_hooks() {
    let tmp = tempfile::tempdir().unwrap();
    let hooks = tmp.path().join(".git/hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    // Two cqs-marked + one foreign hook. Seeding with HOOK_MARKER_CURRENT
    // works because the classifier at line 176 checks HOOK_MARKER_PREFIX
    // (= "# cqs:hook") and HOOK_MARKER_CURRENT (= "# cqs:hook v1") contains
    // the prefix.
    std::fs::write(hooks.join("post-checkout"), HOOK_MARKER_CURRENT).unwrap();
    std::fs::write(hooks.join("post-merge"), HOOK_MARKER_CURRENT).unwrap();
    std::fs::write(hooks.join("post-rewrite"), "#!/bin/sh\necho user").unwrap();

    // Need to factor cmd_uninstall to take an explicit git_dir param
    // (or use a path-override env var). See note below.
    let report = do_uninstall(&hooks).unwrap();
    assert_eq!(report.removed.len(), 2);
    assert_eq!(report.skipped_foreign, vec!["post-rewrite".to_string()]);
}

#[test]
#[cfg(unix)]
fn cmd_fire_writes_dirty_marker_when_daemon_absent() {
    let tmp = tempfile::tempdir().unwrap();
    let cqs_dir = tmp.path().join(".cqs");
    // No socket — daemon unreachable. Same path-override pattern.
    let report = do_fire(&cqs_dir, "post-checkout", vec![], false).unwrap();
    assert!(!report.sent_to_daemon);
    assert!(cqs_dir.join(".dirty").exists());
    assert_eq!(report.dirty_marker, Some(cqs_dir.join(".dirty")));
}

#[test]
fn cmd_status_classifies_three_hook_states() {
    let tmp = tempfile::tempdir().unwrap();
    let hooks = tmp.path().join(".git/hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    // installed: written with marker (HOOK_MARKER_CURRENT contains HOOK_MARKER_PREFIX)
    std::fs::write(hooks.join("post-checkout"), HOOK_MARKER_CURRENT).unwrap();
    // foreign: no cqs marker — body never matches HOOK_MARKER_PREFIX
    std::fs::write(hooks.join("post-merge"), "#!/bin/sh\nuser stuff").unwrap();
    // missing: post-rewrite absent on disk

    let report = do_hook_status(&hooks).unwrap();
    assert_eq!(report.installed, vec!["post-checkout".to_string()]);
    assert_eq!(report.foreign, vec!["post-merge".to_string()]);
    assert_eq!(report.missing, vec!["post-rewrite".to_string()]);
}
```

### Note: factor out path-aware helpers

The current `cmd_uninstall` / `cmd_fire` / `cmd_status` call `find_project_root()` inside; tests need explicit-path equivalents. Factor them:

```rust
fn cmd_uninstall(json: bool) -> Result<()> {
    let root = find_project_root();
    let git_dir = locate_git_hooks_dir(&root)?;
    let report = do_uninstall(&git_dir)?;
    emit(&report, json)?;
    Ok(())
}

fn do_uninstall(git_dir: &Path) -> Result<UninstallReport> {
    // ... existing body, now takes git_dir as param ...
}
```

Same for `cmd_fire` (extract `do_fire(cqs_dir, name, args, dirty_path)`) and `cmd_status` (extract `do_hook_status(git_dir, cqs_dir_for_daemon_check)` — note the helper is `do_hook_status`, distinct from the public `cmd_status`).

### Verification

- `cargo test --features gpu-index --lib hook -- cmd_uninstall cmd_fire cmd_status`.

---

## P2: RB-10 — `now_unix_secs()` swallows clock-before-epoch errors as `0`

**Files:** `src/watch_status.rs:226-231`
**Effort:** ~30 minutes
**Why:** `SystemTime::now().duration_since(UNIX_EPOCH).map(...).unwrap_or(0)` silently returns `0` (= 1970-01-01) on a clock-before-epoch error. Reachable on real systems: pre-NTP-sync VMs/Pis, WSL hypervisor pause-resume, misconfigured Windows RTC. When this happens, every WatchSnapshot publishes `snapshot_at: 0`, downstream freshness logic (`now - snapshot_at > threshold`) decides every snapshot is "56 years stale" and marks the daemon dead.

### Current code

```rust
// src/watch_status.rs:226-231
fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
```

### Replacement / approach

**Reframed during verification:** changing `WatchSnapshot.snapshot_at: i64` to `Option<i64>` is a wire-shape change with a non-trivial blast radius. The original prompt said "wire shape contract changes" but didn't enumerate the impacted sites; here is the full list.

Change `snapshot_at` to `Option<i64>` so missing-clock is unrepresentable as a valid timestamp, and warn-once on bad-clock:

1. Update `WatchSnapshot.snapshot_at` field at `src/watch_status.rs:109` from `i64` to `Option<i64>`.

2. Replace `now_unix_secs` body:

```rust
fn now_unix_secs() -> Option<i64> {
    use std::sync::OnceLock;
    static WARNED_BAD_CLOCK: OnceLock<()> = OnceLock::new();

    match SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_secs()).ok(),
        Err(e) => {
            // RB-10: surface the bad-clock condition once per process so
            // journalctl operators can correlate stale snapshots with
            // NTP-pre-sync boot.
            WARNED_BAD_CLOCK.get_or_init(|| {
                tracing::warn!(
                    error = %e,
                    "system clock is before UNIX_EPOCH — snapshot_at will be None \
                     until NTP sync; check `timedatectl` / `chronyc tracking`",
                );
            });
            None
        }
    }
}
```

3. Update **all 5 sites** that touch `snapshot_at` (compile errors will not surface them all in one pass — `i64` -> `Option<i64>` is silent at most assignments because of `Option::Some(x)` wrapping, so enumerate explicitly):

   **Production (3 sites in `watch_status.rs`):**
   - `src/watch_status.rs:109` — field declaration: `pub snapshot_at: Option<i64>,`
   - `src/watch_status.rs:128` — `WatchSnapshot::unknown`: `snapshot_at: now_unix_secs(),` (already returns `Option<i64>`, no change needed once signature is updated)
   - `src/watch_status.rs:221` — `WatchSnapshot::compute`: same — `snapshot_at: now_unix_secs(),`

   **Test fixtures (3 sites in `daemon_translate.rs`):**
   - `src/daemon_translate.rs:1032` — change `snapshot_at: 1_734_120_500,` to `snapshot_at: Some(1_734_120_500),`
   - `src/daemon_translate.rs:1237` — same
   - `src/daemon_translate.rs:1313` — same

   **Batch handler validator (1 site):**
   - `src/cli/batch/handlers/misc.rs:638-639` — the `obj.contains_key("snapshot_at")` assertion still holds because `null` is still a key (serde serializes `Option::None` as JSON `null`, key present). Verify the test's intent: if it's checking "field is present in JSON regardless of value" the existing line is correct. If it's checking "snapshot_at is a number" add a follow-up `obj["snapshot_at"].is_number()` for the healthy-clock path. Recommend the latter — leave the contains_key check, add `is_number()` for the success-path test.

4. Downstream JSON consumers will see `"snapshot_at": null` instead of `0` on a bad clock — the wire shape contract changes from "always populated, may be 1970" to "Some when the clock is sane." This is intentional; the previous shape was a lie and consumers comparing `now - snapshot_at > threshold` were already buggy.

### Verification

- `cargo build --features gpu-index` (must build cleanly after all 7 sites are updated).
- `cargo test --features gpu-index --lib watch_status`.
- `cargo test --features gpu-index --lib daemon_translate` (test fixtures must compile).
- `cargo test --features gpu-index --lib batch -- snapshot_envelope_shape` (the misc.rs validator at line 638).
- Verify wire-shape change: `cqs status --watch-fresh --json` shows `"snapshot_at": <int>` on a healthy system, `null` only on bad clock.

---

## P2: TC-HAP-1.30.1-3 — `cmd_status` 6-row behavior matrix unpinned

**Files:** `src/cli/commands/infra/status.rs:38-103`, plus new `tests/cli_status_test.rs`
**Effort:** ~60 minutes
**Why:** `cmd_status` ships zero tests on the CLI body. The docstring promises a 6-row behaviour matrix (no flag, --watch-fresh, --wait without --watch-fresh, etc.) — none of the exit codes or output shapes are pinned. A regression that swaps `state:` and `modified_files=` lines goes undetected.

### Approach

Add `tests/cli_status_test.rs` (mirrors `tests/cli_ref_test.rs` shape — XDG-isolated, `assert_cmd`, gated behind `slow-tests`):

```rust
//! Integration tests for `cqs status`. Pins the 6-row behaviour matrix
//! from the cmd_status docstring.

#[cfg(all(unix, feature = "slow-tests"))]
#[test]
fn cqs_status_no_flag_exits_one_with_gate_message() {
    let tmp = tempfile::tempdir().unwrap();
    let output = assert_cmd::Command::cargo_bin("cqs")
        .unwrap()
        .arg("status")
        .env("XDG_RUNTIME_DIR", tmp.path())
        .current_dir(tmp.path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--watch-fresh"));
}

#[cfg(all(unix, feature = "slow-tests"))]
#[test]
fn cqs_status_wait_without_watch_fresh_exits_one() {
    let tmp = tempfile::tempdir().unwrap();
    let output = assert_cmd::Command::cargo_bin("cqs")
        .unwrap()
        .args(["status", "--wait"])
        .env("XDG_RUNTIME_DIR", tmp.path())
        .current_dir(tmp.path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
}

#[cfg(all(unix, feature = "slow-tests"))]
#[test]
fn cqs_status_watch_fresh_without_daemon_exits_one_with_friendly_msg() {
    let tmp = tempfile::tempdir().unwrap();
    let output = assert_cmd::Command::cargo_bin("cqs")
        .unwrap()
        .args(["status", "--watch-fresh"])
        .env("XDG_RUNTIME_DIR", tmp.path())
        .current_dir(tmp.path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("cqs:"));
}

// Daemon-up paths use UnixListener mock pattern from
// daemon_translate.rs::tests::wait_for_fresh_returns_fresh_on_first_poll.
#[cfg(all(unix, feature = "slow-tests"))]
#[test]
fn cqs_status_watch_fresh_with_fresh_daemon_exits_zero_pins_text_format() {
    // Spin up UnixListener mock at $XDG_RUNTIME_DIR/cqs-<hash>.sock
    // Respond to one status request with Fresh envelope.
    // Run `cqs status --watch-fresh`, assert exit 0.
    // Pin stdout: must contain "state: fresh" on its own line.
}
```

### Verification

- `cargo test --features gpu-index,slow-tests --test cli_status_test`.

---

## Summary

**32 P2 findings → 22 distinct fix prompts after grouping.**

### Bundled prompts (5 bundles absorbing 15 findings)

1. **P2-bundle-wait-fresh** — RB-9, EH-V1.30.1-2, OB-V1.30.1-8, TC-HAP-1.30.1-5, TC-ADV-1.30.1-4 (5 findings → 1 prompt)
2. **P2-bundle-reconcile-stat** — EH-V1.30.1-7, TC-ADV-1.30.1-5, TC-ADV-1.30.1-6 (3 findings → 1 prompt)
3. **P2-bundle-watch-status-machine** — OB-V1.30.1-3, TC-HAP-1.30.1-8 (2 findings → 1 prompt)
4. **P2-bundle-eval-gate** — OB-V1.30.1-6, TC-HAP-1.30.1-4, TC-HAP-1.30.1-7 (3 findings → 1 prompt)
5. **P2-bundle-rb1-rb6** — RB-1, RB-6 (2 findings → 1 prompt)

### Single-issue prompts (17 distinct fixes, one per finding)

6. EH-V1.30.1-1 — Parse failure leaves stale chunks
7. EH-V1.30.1-8 — `try_init_embedder` Err strands HNSW dirty without observability
8. AC-V1.30.1-3 — BFS cap-check skips score-bump
9. AC-V1.30.1-9 — `daemon_socket_path` uses `DefaultHasher`
10. AC-V1.30.1-10 — `incremental_count = 0` reset loses delta
11. API-V1.30.1-1 — `cqs status --wait` success envelope but exits 1
12. API-V1.30.1-5 — `daemon_*` Result<T, String> (folded into bundle-wait-fresh)
13. API-V1.30.1-10 — `WatchSnapshot.idle_secs` frozen at compute time
14. OB-V1.30.1-9 — `process_file_changes` uses println
15. OB-V1.30.1-10 — `serve::search` info-logs full query
16. PB-V1.30.1-1 — `--no-auth` warning misses 0.0.0.0/::
17. PB-V1.30.1-3 — Windows `tasklist` substring-matches localized output
18. PB-V1.30.1-7 — `cqs hook fire` Windows-native marker has no consumer
19. SEC-V1.30.1-3 — callgraph-3d.js innerHTML XSS gap
20. SEC-V1.30.1-4 — `tag_user_code_trust_level` shape-coupled
21. DS-V1.30.1-D1 — `cqs index --force` reopen leaves stale pending_rebuild
22. DS-V1.30.1-D2 — Reconcile bypasses max_pending_files cap
23. DS-V1.30.1-D5 — `.cqs/.dirty` marker not atomic
24. DS-V1.30.1-D7 — HNSW rollback orphans .bak files
25. TC-HAP-1.30.1-2 — `cmd_uninstall`/`cmd_fire`/`cmd_hook_status` zero tests
26. TC-HAP-1.30.1-3 — `cmd_status` 6-row matrix unpinned
27. RB-10 — `now_unix_secs()` swallows clock-before-epoch as 0

Item 12 (API-V1.30.1-5) refactors three RPC return types as part of bundle-wait-fresh's `DaemonStatusError` introduction; the standalone prompt above documents the call-site impact. Three additional cross-reference stubs at #8/#9/#10 of the document point to bundles (RB-9 → bundle-wait-fresh; RB-1, RB-6 → bundle-rb1-rb6).

**Net distinct prompts: 22** (5 bundles + 17 single-issue, with API-V1.30.1-5 folded inside bundle-wait-fresh and three RB cross-reference stubs).
## P2 Verification Report

Generated 2026-04-28 against actual source. 22 distinct P2 prompts checked (5 bundles + 17 single-issue + 3 cross-reference stubs).

> **Note:** Append this section to `audit-fix-prompts.md` immediately before `# v1.30.1 Audit — P3 Fix Prompts` at line 3943. Edit-tool insertion was blocked by a `PreToolUse` hook resolution error (`docs/.claude/hooks/pre-edit-impact.py` missing) so the report was written to this side file instead.

### P2-bundle-wait-fresh: VERIFIED

Code at `daemon_translate.rs:637-679` matches; eval gate at `eval/mod.rs:246-264` matches. Cited connect-stage warn at line 438-441 verified. Bundle covers all 5 finding IDs (RB-9, EH-V1.30.1-2, OB-V1.30.1-8, TC-HAP-1.30.1-5, TC-ADV-1.30.1-4). Caller sweep is complete (3 production callers: `infra/ping.rs`, `infra/hook.rs:311+369`, `infra/status.rs:68`; plus 5 in-module test sites). Note: cited line range "623-678" is off-by-12; actual range is 635-679, but content matches.

### P2-bundle-reconcile-stat: VERIFIED

`reconcile.rs:116-127` matches verbatim. Parallel pattern at `reindex.rs:501-509` confirmed. All 3 finding IDs covered (EH-V1.30.1-7, TC-ADV-1.30.1-5, TC-ADV-1.30.1-6).

### P2-bundle-watch-status-machine: NEEDS FIX

Issues:
- The proposed test `compute_with_rebuild_in_flight_returns_rebuilding` and `compute_rebuilding_takes_precedence_over_pending_files` are **redundant** — `watch_status.rs:278-284` already has `rebuild_dominates_over_stale_files` which pins `rebuild_in_flight=true, pending_files_count=5 -> state == Rebuilding`. The Why section's claim "Rebuilding... has no test through compute()" is false.
- Both 2 finding IDs are still substantively covered (transition logging is the load-bearing fix); just the test list overstates.

Correction: drop both proposed Rebuilding tests (already covered) or rename them to something orthogonal, e.g. a test pinning the prev/next transition log line. The main transition-emission fix is sound.

### P2-bundle-eval-gate: VERIFIED

`eval/mod.rs:219-275` matches verbatim. `env_disables_freshness_gate` exists at line 282. `serial_test = "3"` available; `#[serial_test::serial(name)]` style matches existing daemon_translate usage. All 3 finding IDs covered.

### P2-bundle-rb1-rb6: VERIFIED

Both `reconcile.rs:99` and `lib.rs:680-685` match verbatim. Both 2 finding IDs covered (RB-1, RB-6).

### EH-V1.30.1-1 (parse failure stale chunks): NEEDS FIX

Issues:
- Function `Store::touch_source_mtime(&Path, i64)` does NOT exist. Prompt says "if it doesn't exist yet, add it" — that's correct framing.
- However, the new helper must call `crate::normalize_path()` on the path before binding to the `WHERE origin = ?` query — otherwise it will fail to match the path-vs-origin string format used by the indexer (see `delete_by_origin` at `store/chunks/crud.rs:614-616` for the canonical pattern).

Correction: add an explicit note that the new `touch_source_mtime` must `crate::normalize_path(origin)` to match the indexer's storage convention.

### EH-V1.30.1-8 (try_init_embedder Err): NEEDS FIX

Issues:
- Title says "`try_init_embedder` Err leaves HNSW dirty" — but `try_init_embedder` returns `Option<Embedder>` (None, not Err). The fix's actual scope (the dirty-flag path at `events.rs:178-185`) is correct, but the title and Why section are wrong about the surfaced symptom.

Correction: rename title to "watch reindex failure leaves HNSW dirty without observability" and revise Why section to state events.rs:178-185 dirty-flag set + reindex_files failure path, not embedder.

### AC-V1.30.1-3 (BFS cap skips score-bump): VERIFIED

`gather.rs:341-378` matches verbatim. Fix correctly moves cap check inside `!visited.contains()` branch.

### AC-V1.30.1-9 (DefaultHasher): NEEDS FIX

Issues:
- Wire-format change: existing `format!("cqs-{:x}.sock", h.finish())` produces variable-length unpadded hex (e.g. `cqs-deadbeef.sock` for shorter values), while the proposed BLAKE3 + 8-byte-truncated hex produces fixed 16-char output. Switching means existing systemd units pointing at the old socket name will not find the new one, requiring operators to restart `cqs-watch` — but the prompt does NOT call out this transition pain.
- The `cli/files.rs:26` wrapper `daemon_socket_path` already delegates to `daemon_translate::daemon_socket_path`, so changing the impl in one place is sufficient (verified).

Correction: add a note that this is a one-time migration: existing `cqs-watch` services will need to be restarted after the upgrade so the daemon binds to the new socket name and CLI clients connect to the same.

### AC-V1.30.1-10 (incremental_count idle reset): VERIFIED

`watch/mod.rs:1175-1182` matches verbatim. Fix is a single line removal.

### API-V1.30.1-1 (status --wait timeout envelope): NEEDS FIX

Issues:
- The replacement uses `error_codes::TIMEOUT` which does NOT exist. `error_codes` module at `cli/json_envelope.rs:125-139` only defines `NOT_FOUND`, `INVALID_INPUT`, `PARSE_ERROR`, `IO_ERROR`, `INTERNAL`.
- Prompt acknowledges `emit_json_error_with_data` doesn't exist and says "add it". OK.

Correction: either add a `Timeout` variant to the `ErrorCode` enum + `error_codes::TIMEOUT` constant (wire shape extension), or use `error_codes::IO_ERROR` (closest existing match), or use a literal `"timeout"` string code. The cleanest fix is extending the enum since this is the first time-budget-exceeded error envelope.

### API-V1.30.1-5: VERIFIED (folded into bundle-wait-fresh)

Cross-reference correctly notes the fold into bundle-wait-fresh. Approach uses `thiserror::Error` derive — `thiserror` is already in deps.

### API-V1.30.1-10 (idle_secs frozen): VERIFIED

Code at `watch_status.rs:101, 219` matches. Fix proposes (b) "document and rename" approach — adds `last_event_unix_secs: i64`. Wire shape extension is consistent. Note: the plumbing instruction `last_event_unix_secs = SystemTime::now()...as_secs() as i64 - last_event.elapsed().as_secs() as i64` is fine but `last_event` is `Instant` not `SystemTime`, so we can't directly compute Unix secs from it — the subtraction approach is the right workaround. Correctly identified.

### OB-V1.30.1-9 (println! in watch): VERIFIED

`events.rs:147-152` matches verbatim. Fix replaces with `tracing::info!`.

### OB-V1.30.1-10 (serve::search query log): VERIFIED

`serve/handlers.rs:189-232` matches verbatim. Demote query-string from info to debug is straightforward.

### PB-V1.30.1-1 (--no-auth wildcard binds): VERIFIED

`serve.rs:27` matches verbatim. Fix uses `IpAddr::is_loopback()` + name fallback for "localhost". Correctly handles 0.0.0.0 / :: as non-loopback.

### PB-V1.30.1-3 (Windows tasklist localized): VERIFIED

`cli/files.rs:59-72` matches verbatim. Switching to `/FO CSV` with column-bounded substring check is sound.

### PB-V1.30.1-7 (Windows .dirty consumer): VERIFIED

`hook.rs:323-332` matches; consumer at `watch/mod.rs:594` confirmed `#[cfg(unix)]`. `cmd_index` at `index/build.rs:23` exists. Fix proposes adding `consume_dirty_marker` helper — sensible.

### SEC-V1.30.1-3 (XSS in callgraph-3d.js): VERIFIED

`callgraph-3d.js:43-58` matches; `escapeHtml` mirror pattern from cluster-3d.js / hierarchy-3d.js confirmed.

### SEC-V1.30.1-4 (tag_user_code shape coupling): VERIFIED

`commands/mod.rs:216-257` matches verbatim. Recursive visitor approach is sound. Tests are well-scoped.

### DS-V1.30.1-D1 (--force reopen pending_rebuild): VERIFIED

`watch/mod.rs:1102-1122` matches; `pending_rebuild` field on `WatchState` confirmed at line 139. `state.pending_rebuild.take()` is a real `Option<PendingRebuild>` field.

### DS-V1.30.1-D2 (reconcile bypasses pending cap): NEEDS FIX

Issues:
- Prompt says "Update both call sites in src/cli/watch/mod.rs:1262-1283" but there are TWO production call sites at `watch/mod.rs:1055` AND `watch/mod.rs:1268`. Only one range is mentioned.
- Test call sites at `reconcile.rs:190, 204, 225, 362, 450` (5 sites) all need updating because the function signature gains a new `cap: usize` parameter. The prompt does not enumerate these.

Correction: enumerate all 7 call sites (2 production in `watch/mod.rs` + 5 in-module tests in `reconcile.rs`) that must be updated when adding the `cap` parameter. The signature change cascades.

### DS-V1.30.1-D5 (.dirty atomic write): NEEDS FIX

Issue:
- The replacement calls `cqs::fs::atomic_replace(&dirty, b"")` — the actual signature at `fs.rs:41` is `atomic_replace(tmp_path: &Path, final_path: &Path) -> io::Result<()>`. It takes TWO `&Path` arguments (tmp + final), NOT a path and bytes.
- This will fail to compile: `b""` is `&[u8; 0]`, not `&Path`.

Correction: write content to a `.dirty.tmp` path first, then call `atomic_replace(&tmp, &final)`:
```rust
let tmp = cqs_dir.join(".dirty.tmp");
std::fs::write(&tmp, b"")?;
cqs::fs::atomic_replace(&tmp, &dirty)?;
```

### DS-V1.30.1-D7 (HNSW rollback .bak orphans): NEEDS FIX

Issues:
- Prompt repeatedly references `save_owned`. The actual function is `save` (verified at `hnsw/persist.rs:208`). No `save_owned` exists.
- The replacement uses `anyhow::anyhow!` and `anyhow::bail!` — but `save` returns `Result<(), HnswError>`, not `Result<()>` (anyhow). Anyhow types will not coerce into HnswError. This would not compile.

Correction: rename all `save_owned` references to `save`. Replace `anyhow::anyhow!(...)` with `HnswError::Internal(format!(...))` and `anyhow::bail!(...)` with `return Err(HnswError::Internal(format!(...)))`. The cited line range 509-553 matches but the function is `save` not `save_owned`.

### TC-HAP-1.30.1-2 (zero tests for hook commands): NEEDS FIX

Issues:
- Function name in title is `cmd_hook_status`, but the actual function is `cmd_status` (verified at `infra/hook.rs:339`). Test name `cmd_hook_status_classifies_three_hook_states` describes a fictitious function.
- Hook content marker check uses `HOOK_MARKER_PREFIX`, not `HOOK_MARKER_CURRENT`. The proposed test seeds files with `HOOK_MARKER_CURRENT` which CONTAINS the prefix, so the test would still pass — but the test code claims to be testing the foreign-vs-marked classifier.

Correction: rename `cmd_hook_status` references to `cmd_status` in the test name and approach. Note that `HOOK_MARKER_PREFIX` is what the code matches; clarify that test seeds using `HOOK_MARKER_CURRENT` work because they contain the prefix.

### RB-10 (now_unix_secs swallows clock errors): NEEDS FIX

Issues:
- Changing `WatchSnapshot.snapshot_at: i64` to `Option<i64>` is a wire-shape break. The prompt says "wire shape contract changes" but does not enumerate the impacted sites. Tests at `daemon_translate.rs:1032, 1237, 1313` use `snapshot_at: 1_734_120_500` (literal i64) and would not compile after the change. `cli/batch/handlers/misc.rs:638-639` validates `snapshot_at` presence in JSON, also affected.
- The Replacement code has stray whitespace inside a backslash-continued string literal — copy-paste artifact.

Correction: enumerate the i64 -> Option<i64> blast radius (5 known sites: `watch_status.rs:109, 128, 221`, `daemon_translate.rs:1032/1237/1313`, `batch/handlers/misc.rs:638-639`) and fix them in the same prompt, or split into a multi-step refactor.

### TC-HAP-1.30.1-3 (cmd_status matrix unpinned): VERIFIED

`status.rs:38-103` matches; flow at lines 41-54 confirms `--wait` without `--watch-fresh` exits 1. `slow-tests` feature exists in Cargo.toml. Tests use `assert_cmd::Command::cargo_bin("cqs")` — convention matches existing CLI tests. Note: existing convention uses file-level `#![cfg(feature = "slow-tests")]`, not per-test `#[cfg(all(unix, feature = "slow-tests"))]` — minor stylistic deviation, but both compile.

### Cross-reference stubs (RB-9, RB-1, RB-6, API-V1.30.1-5): VERIFIED

All four cross-reference entries correctly point to their merging bundle. No content to verify; redirects only.

---

### Summary

- **VERIFIED:** 13 prompts (2 bundles + 8 single-issue + 3 cross-reference stubs)
- **NEEDS FIX:** 9 prompts (1 bundle partially + 8 single-issue with concrete errors)

### Three most concerning issues (severity-ordered)

1. **DS-V1.30.1-D5 — `cqs::fs::atomic_replace` API mismatch.** The proposed call `atomic_replace(&dirty, b"")` will not compile; the function takes `(tmp_path: &Path, final_path: &Path)`, not `(path, bytes)`. This is the same class of error as the P1 verifier flagged: a fix that uses a fictitious API shape.

2. **DS-V1.30.1-D7 — Wrong function name + wrong error type.** Prompt repeatedly says `save_owned` (doesn't exist; actual is `save`) AND uses `anyhow::anyhow!` / `anyhow::bail!` in a function that returns `Result<(), HnswError>` (not anyhow). Two separate compile errors stacked.

3. **API-V1.30.1-1 — `error_codes::TIMEOUT` doesn't exist.** The replacement code references a constant that's never been defined. The error_codes module has only NOT_FOUND, INVALID_INPUT, PARSE_ERROR, IO_ERROR, INTERNAL.

### Bundle completeness

All 5 bundles cover every merged finding ID — no IDs slipped through. The watch-status-machine bundle's tests are partly redundant with existing tests but the merged finding IDs are still substantively addressed.
# v1.30.1 Audit — P3 Fix Prompts

P3 = easy + low impact, fix if time. 78 findings collapsed into ~32 prompts via grouping.

Generated 2026-04-28. Cross-referenced with audit-triage.md and audit-findings.md.

---

## P3-DOC-1 — Five doc-only edits (no trust claim shifts)

**Reframed during verification:** anchors corrected — PRIVACY block is 18-22 (three legacy lines, not one); SECURITY platform-native paths live in the read-access table at 111-115. Item 4's existing line already documents the 250 ms poll cap; the rewrite still adds the more-discoverable "30 s default budget" wording.

**Files:** `README.md:540-569`, `CONTRIBUTING.md:149-340`, `PRIVACY.md:18-22`, `SECURITY.md:111-115`, `README.md:219-220`, `CHANGELOG.md:71`, `ROADMAP.md:131`
**Effort:** ~15 min total
**Bundles:** DOC-V1.30.1-2, DOC-V1.30.1-3, DOC-V1.30.1-5, DOC-V1.30.1-6, DOC-V1.30.1-9

**Fixes:**

1. **README canonical command list** (`README.md:540-569`): insert two new rows after line 568 (before the `cqs completions` row at 569):
   ```
   - `cqs hook install/uninstall/status/fire` - manage `.git/hooks/post-{checkout,merge,rewrite}` for watch-mode reconciliation. Idempotent; respects third-party hooks via marker check (#1182)
   - `cqs status --watch-fresh [--wait [--wait-secs N]]` - report watch-loop freshness; `--wait` blocks until `state == fresh` (default 30 s, capped at 600 s) (#1182)
   ```

2. **CONTRIBUTING Architecture Overview** (`CONTRIBUTING.md:149-340`): append missing entries — `eval/` (mod.rs + schema.rs), `watch_status.rs`, `daemon_translate.rs`, `fs.rs`, `limits.rs`, `aux_model.rs`, `cli/commands/serve.rs`. In `cli/commands/infra/`: `hook.rs`, `model.rs`, `ping.rs`, `slot.rs`, `status.rs`. In `cli/commands/index/`: `umap.rs`. In `cli/watch/`: `reconcile.rs - Layer 2 periodic full-tree reconciliation (#1182)`.

3. **PRIVACY/SECURITY platform-native cache paths** — extend (don't replace) the legacy `~/.cache/cqs/` block:
   - **PRIVACY.md:18-22**: the legacy block is three lines (`embeddings.db`, `query_cache.db`, `query_log.jsonl`). Add a leading platform-resolution note above it:
     ```
     The legacy cache root resolves per platform: Linux `$XDG_CACHE_HOME/cqs/` or `~/.cache/cqs/`; macOS `~/Library/Caches/cqs/`; Windows `%LOCALAPPDATA%\cqs\`. The three files below live under that root.
     ```
     Then update the `rm -rf` block lower in the file to include `~/Library/Caches/cqs/` and `%LOCALAPPDATA%\cqs\` alongside the existing Linux path.
   - **SECURITY.md:111-115** (read access table for `~/.cache/cqs/embeddings.db`, `query_cache.db`): add a footnote row noting that on macOS the path is `~/Library/Caches/cqs/...` and on Windows `%LOCALAPPDATA%\cqs\...`. Mirror in the write-access table at 134-136.

4. **README Watch Mode default budget** (`README.md:219-220`): existing line 219 is `cqs status --watch-fresh --wait` with `(250 ms poll, capped at 600 s)`. Rewrite to lead with the default 30 s budget for discoverability:
   ```
   cqs status --watch-fresh --wait                     # block until fresh (default 30 s budget, 250 ms poll, capped at 600 s)
   cqs status --watch-fresh --wait --wait-secs 600     # extend up to the 600 s cap
   ```

5. **CHANGELOG/ROADMAP cache subcommand list** (`CHANGELOG.md:71`, `ROADMAP.md:131`): change `{stats,prune,compact}` to `{stats,clear,prune,compact}`.

---

## P3-DOC-2 — Skip: subsumed

- `DOC-V1.30.1-8` (CONTRIBUTING test count) — **Covered by:** P3-DOC-1 item 2 (the architecture overview update); no separate action needed per the finding's own note.

---

## P3-CQ-1 — eval Timeout error message: include drop/saturation signals

**File:** `src/cli/commands/eval/mod.rs:248-255`
**Effort:** ~5 min
**Finding:** CQ-V1.30.1-3

**Fix:**
```rust
// before (lines 248-255):
FreshnessWait::Timeout(snap) => anyhow::bail!(
    "watch index is still stale after {budget_secs}s wait \
     (modified_files={}, pending_notes={}, rebuild_in_flight={}); \
     wait longer with --require-fresh-secs N or skip with --no-require-fresh",
    snap.modified_files,
    snap.pending_notes,
    snap.rebuild_in_flight,
),

// after — add dropped_this_cycle and delta_saturated:
FreshnessWait::Timeout(snap) => anyhow::bail!(
    "watch index is still stale after {budget_secs}s wait \
     (modified_files={}, pending_notes={}, rebuild_in_flight={}, \
     dropped_this_cycle={}, delta_saturated={}); \
     wait longer with --require-fresh-secs N or skip with --no-require-fresh",
    snap.modified_files,
    snap.pending_notes,
    snap.rebuild_in_flight,
    snap.dropped_this_cycle,
    snap.delta_saturated,
),
```

---

## P3-CQ-2 — Dedupe `ort_err` helpers across embedder/reranker/splade

**Files:** `src/embedder/provider.rs:14`, `src/reranker.rs:100`, `src/splade/mod.rs:27`
**Effort:** ~10 min
**Finding:** CQ-V1.30.1-5

**Fix:** Promote `embedder::provider::ort_err` to a crate-private free function (e.g., `crate::ort_helpers::ort_to_inference<E: From<String>>`) and add `From<String>` impls (or per-enum constructors) on `RerankerError::Inference` and `SpladeError::Inference`. Then delete the two duplicate helpers and reuse the central one. Saves ~9 LOC and removes the cross-module rationale comment at `reranker.rs:99`.

---

## P3-CQ-3 — Drop redundant `--no-auth` localhost warning at boot

**File:** `src/cli/commands/serve.rs:27-37`
**Effort:** ~5 min
**Finding:** CQ-V1.30.1-6

**Fix:** The CLI-side warning at `cmd_serve:27-37` is silent for the most-common localhost+`--no-auth` footgun, while `serve/mod.rs:162-165` already emits a structured warning unconditionally. Drop the early `if no_auth && bind != "127.0.0.1" && bind != "localhost" && bind != "::1"` block entirely; rely on the `run_server` warning. Don't carry both. (See P3-PB-1 for the wildcard-bind tightening.)

---

## P3-API-1 — Renames, defaults, and shape unifications across `cqs status`, `cqs eval`, daemon RPCs (8 quick wins)

**Bundles:** API-V1.30.1-2, API-V1.30.1-3, API-V1.30.1-4, API-V1.30.1-6, API-V1.30.1-7, API-V1.30.1-8, API-V1.30.1-9, API-V1.30.1-10
**Effort:** ~30-45 min total

| ID | File:Line | Fix |
|----|-----------|-----|
| API-V1.30.1-2 | `definitions.rs:792`, `eval/mod.rs:85` | Doc-only fix: append a one-liner in each `--help` cross-referencing the other surface. ("Note: `cqs eval --require-fresh-secs` has the same semantics; default differs by use case.") No flag rename — that breaks scripts. |
| API-V1.30.1-3 | `eval/mod.rs:79-80` | Add a `///` line on `no_require_fresh`: "Off-switch for the default-on `--require-fresh` gate. Set `CQS_EVAL_REQUIRE_FRESH=0` for the env equivalent." Don't rename — this is the only `--no-X` flag intentionally because it gates a default-on safety surface. |
| API-V1.30.1-4 | `daemon_translate.rs:236`, `watch_status.rs:105` | Add `#[serde(alias = "last_synced_at")]` to `PingResponse.last_indexed_at`, OR `#[serde(alias = "last_indexed_at")]` to `WatchSnapshot.last_synced_at`. Pick one canonical name in docs but keep both deserializing. |
| API-V1.30.1-6 | `daemon_translate.rs:517-519` | Drop the `queued: bool` field — `Ok(...)` already conveys "queued". Delete the field, drop the doc lie about "always true", bump `JSON_OUTPUT_VERSION`. |
| API-V1.30.1-7 | `watch_status.rs:181-193` | Add a `pub fn new(...)` constructor on `WatchSnapshotInput<'a>` that takes the named fields and fills `_marker: PhantomData`. Make the field set encapsulated. Migrate the one caller in `cli/watch/mod.rs:165` (inside `publish_watch_snapshot`; line 1303 is the `publish_watch_snapshot(...)` invocation, not the struct construction) to `WatchSnapshotInput::new(...)`. (Don't drop the lifetime today — it's load-bearing for future borrow-only fields per the comment.) |
| API-V1.30.1-8 | `cli/commands/infra/status.rs:41-54` | Change the no-flag `eprintln!` to a stderr hint: `eprintln!("cqs status: hint: try --watch-fresh --wait")` and keep `exit(1)`. Document in `--help` that `--watch-fresh` is currently the only mode. |
| API-V1.30.1-9 | `watch_status.rs:51-60` | Add `impl std::fmt::Display for FreshnessState { fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(self.as_str()) } }`. Two-line addition. Lets `tracing::info!(state = %snap.state)` work. |
| API-V1.30.1-10 | `watch_status.rs:101,219` | Update doc comment on `idle_secs`: "Seconds since last filesystem event **at snapshot time** (not live; see `last_event_at` for live-idle computation)." Optionally add `last_event_at: Option<i64>` so consumers can derive `now() - last_event_at` for live idle. Pure documentation fix is the cheap path. |

---

## P3-API-2 — Skip: covered by P2

- **API-V1.30.1-5** (`Result<T, String>` on daemon RPCs): **Covered by:** the P2 entry of the same ID. Not duplicated as a P3 prompt.

---

## P3-EH-1 — Replace four `unwrap_or_default()` / `let _ =` swallow-error sites with explicit warns

**Bundles:** EH-V1.30.1-3, EH-V1.30.1-4, EH-V1.30.1-5, EH-V1.30.1-6
**Effort:** ~15 min total

| ID | File:Line | Fix |
|----|-----------|-----|
| EH-V1.30.1-3 | `src/cli/dispatch.rs:207` | Replace `let resolved_slot = cqs::slot::resolve_slot_name(...).ok();` with `match` — on Err: `tracing::warn!(error = %e, slot = ?cli.slot, "slot resolution failed when looking up persisted model intent"); None`. |
| EH-V1.30.1-4 | `src/cli/commands/infra/doctor.rs:923` | Replace `cqs::slot::list_slots(...).unwrap_or_default()` with explicit `match`: on Err, emit `tracing::warn!` AND add a `slot_listing_error: Option<String>` field to the doctor JSON envelope. The whole point of `cqs doctor` is to surface failures. |
| EH-V1.30.1-5 | `src/cli/commands/index/build.rs:863-867` | Replace both `try_model_config().map(...).unwrap_or_default()` and `store.chunk_count().unwrap_or(0)` with `?` propagation (the index command returns `Result`). Or include explicit `error` field in the JSON envelope. |
| EH-V1.30.1-6 | `src/reranker.rs:524` | Replace `let _ = std::fs::write(&marker, &expected_marker);` with `if let Err(e) = std::fs::write(&marker, &expected_marker) { tracing::warn!(error = %e, path = %marker.display(), "Failed to write reranker verification marker — next launch will re-verify checksums"); }`. Two lines. |

---

## P3-OB-1 — Demote per-search `tracing::info!` spam to `debug!`

**Bundles:** OB-V1.30.1-1, OB-V1.30.1-2
**Effort:** ~5 min
**Files:** `src/search/router.rs:469-474, 491-496, 549-554` (SPLADE routing) + `:1146-1150` (centroid Unknown-gap)

**Fix:** Demote four call sites from `tracing::info!` to `tracing::debug!`. The existing entry `info_span!`s already provide traceability when the operator opts into debug logs.

| Line range | Current | Replacement |
|------------|---------|-------------|
| `:469-474` | `tracing::info!(category, alpha, source, "SPLADE routing")` | `tracing::debug!(category, alpha, source, "SPLADE routing")` |
| `:491-496` | same shape | demote to `debug!` |
| `:549-554` | same shape | demote to `debug!` |
| `:1146-1150` | `tracing::info!(centroid_category, margin, "centroid filled Unknown gap")` | demote to `debug!` |

---

## P3-OB-2 — Add closing tracing event to `wait_for_fresh` (entry/timeout/no-daemon)

**File:** `src/daemon_translate.rs:660-679`
**Effort:** ~5 min
**Finding:** OB-V1.30.1-4

**Fix:** Before each terminal `return` in `wait_for_fresh`, add an info event:
```rust
let start = std::time::Instant::now();
// ... existing loop ...

// success path:
tracing::info!(elapsed_ms = start.elapsed().as_millis() as u64, modified_files = snap.modified_files, "wait_for_fresh: index reached Fresh");
return FreshnessWait::Fresh(snap);

// timeout path:
tracing::info!(elapsed_ms = start.elapsed().as_millis() as u64, modified_files = snap.modified_files, pending_notes = snap.pending_notes, rebuild_in_flight = snap.rebuild_in_flight, "wait_for_fresh: timeout — index still stale");
return FreshnessWait::Timeout(snap);

// no-daemon path:
tracing::info!(error = %msg, "wait_for_fresh: daemon unreachable");
return FreshnessWait::NoDaemon(msg);
```

---

## P3-OB-3 — Reason field on `enforce_auth` 401 warn

**File:** `src/serve/auth.rs:389-401, 269-321`
**Effort:** ~10 min
**Finding:** OB-V1.30.1-5

**Fix:** Change `AuthOutcome::Unauthorized` to carry a low-cardinality reason enum:
```rust
enum UnauthorizedReason { MissingAll, BearerMismatch, CookieMismatch, QueryParamMismatch }

// in check_request, replace bare `AuthOutcome::Unauthorized` with `Unauthorized(UnauthorizedReason::<which one fired>)`
// in the 401 warn at :389-401:
tracing::warn!(method = %req.method(), path = %req.uri().path(), reason = ?reason, "serve: rejected unauthenticated request");
```

---

## P3-OB-4 — Entry span + final-decision info on `require_fresh_gate`

**File:** `src/cli/commands/eval/mod.rs:219-275`
**Effort:** ~5 min
**Finding:** OB-V1.30.1-6

**Fix:** Wrap the function body in:
```rust
let _span = tracing::info_span!("require_fresh_gate", wait_secs).entered();
```
After `wait_for_fresh` returns, emit one structured event before the bail/Ok path:
```rust
tracing::info!(outcome = "fresh"|"timeout"|"no_daemon", elapsed_ms, modified_files = snap.modified_files, "require_fresh_gate: resolved");
```

---

## P3-OB-5 — `elapsed_ms` field on `daemon_reconcile` and GC walks

**Files:** `src/cli/watch/reconcile.rs:63-148`, `src/cli/watch/gc.rs:103-180,195-243`
**Effort:** ~5 min
**Finding:** OB-V1.30.1-7

**Fix:** Capture `let start = std::time::Instant::now()` at function entry; include `elapsed_ms = start.elapsed().as_millis() as u64` in the terminal `tracing::info!` for `run_daemon_reconcile`, `run_daemon_startup_gc`, and `run_daemon_periodic_gc`. Pattern matches what HNSW build sites already do.

---

## P3-OB-6 — Skip: covered by P2

- **OB-V1.30.1-8** (daemon_status connect-warn loop): **Covered by:** P2 entry of same ID — same fix point, broader scope.
- **OB-V1.30.1-9** (println! in process_file_changes): **Covered by:** P2 entry of same ID.
- **OB-V1.30.1-10** (serve::search query at info): **Covered by:** P2 entry of same ID.

---

## P3-TC-ADV-1 — Bundle: 5 adversarial test additions for serve/auth + daemon

**Bundles:** TC-ADV-1.30.1-1, TC-ADV-1.30.1-2, TC-ADV-1.30.1-3, TC-ADV-1.30.1-9, TC-ADV-1.30.1-10
**Effort:** ~25 min total

Add to `src/serve/auth.rs::tests`:

```rust
#[test]
fn try_from_string_accepts_long_alphabet_input_today() {
    // Pin current shape — no MAX_TOKEN_LEN cap exists.
    // If a cap is ever added, this test should invert.
    let long = "a".repeat(10_240);
    assert!(AuthToken::try_from_string(long).is_ok());
}

#[test]
fn auth_query_wrong_cookie_right_authenticates_via_cookie_no_redirect() {
    // Pin: cookie wins over query. Stale ?token= survives in URL bar.
    // SEC-7: this test pins the leakage gap. Invert when CQ-V1.30.1-4 lands.
    // ... build req with valid cookie + ?token=wrong ...
    // assert AuthOutcome::Ok (NOT OkViaQueryParam)
}

#[test]
fn auth_query_right_cookie_wrong_redirects_and_overwrites_cookie() {
    // ... build req with wrong cookie + ?token=right ...
    // assert AuthOutcome::OkViaQueryParam (current behavior, regression-pin)
}

#[test]
fn auth_two_cookies_with_same_name_uses_first_occurrence() {
    // RFC 6265 allows duplicates; current code matches first.
    // ... build req with `Cookie: cqs_token_8080=wrong; cqs_token_8080=right` ...
    // assert 401 (first wrong one matches first; pin behavior)
}

#[test]
fn bearer_lowercase_scheme_returns_401_today() {
    // RFC 6750 §2.1 says case-insensitive; current code is strict.
    // Pin current 401 behavior; invert when grammar is relaxed.
    // ... build req with `Authorization: bearer <token>` ...
    // assert 401
}

#[test]
fn bearer_double_space_returns_401_today() {
    // ... build req with `Authorization: Bearer  <token>` ...
    // assert 401
}

#[test]
fn bearer_no_separator_returns_401_today() {
    // ... build req with `Authorization: Bearer<token>` ...
    // assert 401
}
```

Add to `src/daemon_translate.rs::tests`:

```rust
#[test]
fn daemon_status_handles_err_envelope_with_no_message() {
    // ... mock daemon writes `{"status":"err"}` (no message field) ...
    // assert err string includes the raw envelope, not "daemon error: daemon error"
}

#[test]
fn daemon_status_handles_err_envelope_with_non_string_message() {
    // ... mock daemon writes `{"status":"err","message": 42}` ...
    // assert err string surfaces the shape mismatch
}

#[test]
fn unwrap_dispatch_payload_distinguishes_envelope_no_data_from_bare_form() {
    // Send `{"data": null, "error": "internal", "version": 1}` — should surface error
    // Signature: fn unwrap_dispatch_payload(output: &serde_json::Value, type_name: &str) -> Result<Value, String>
    let v = serde_json::json!({"data": null, "error": "internal", "version": 1});
    let result = unwrap_dispatch_payload(&v, "TestType");
    assert!(result.is_err());  // not silently passing wrapper through
}
```

---

## P3-TC-ADV-2 — `env_disables_freshness_gate`: rewrite to call function with real env

**File:** `src/cli/commands/eval/mod.rs:282-290, 405-432`
**Effort:** ~10 min
**Finding:** TC-ADV-1.30.1-7

**Fix:** Rewrite the test (currently re-implements function body inline) to actually call `env_disables_freshness_gate()` with `serial_test::serial(cqs_eval_require_fresh_env)`. Use the `unsafe` env-var save/restore pattern from `daemon_translate.rs::tests` (e.g., `reconcile_enabled_default_true`). Cover at minimum:

```rust
#[test]
#[serial_test::serial(cqs_eval_require_fresh_env)]
fn env_disables_freshness_gate_real_env() {
    // SAFETY: serial_test guards env-var collisions.
    unsafe { std::env::remove_var("CQS_EVAL_REQUIRE_FRESH"); }
    assert!(!env_disables_freshness_gate(), "unset = gate stays on");

    for (val, expected) in [
        ("0", true), ("false", true), ("no", true), ("off", true),
        ("  off  ", true),    // whitespace
        ("1", false), ("true", false),
        ("garbage", false), ("", false),    // unknown / empty
    ] {
        unsafe { std::env::set_var("CQS_EVAL_REQUIRE_FRESH", val); }
        assert_eq!(env_disables_freshness_gate(), expected,
                   "value {val:?}: expected {expected}");
    }
    unsafe { std::env::remove_var("CQS_EVAL_REQUIRE_FRESH"); }
}
```

Drop the inline-body test at `:405-432`.

---

## P3-TC-ADV-3 — Skip: covered by P1

- **TC-ADV-1.30.1-8** (`delta_saturated=true → Fresh` test): **Covered by:** the CQ-V1.30.1-2 P1 fix, which changes the state machine so this test then asserts `Stale`. Add the test there, not separately.

---

## P3-RB-1 — `wait_for_fresh` defensive cap on `wait_secs`

**File:** `src/daemon_translate.rs:660-662`
**Effort:** ~3 min
**Finding:** RB-2

**Fix:**
```rust
// before:
let deadline = std::time::Instant::now() + std::time::Duration::from_secs(wait_secs);

// after — defensive cap to prevent Instant+Duration overflow:
let wait_secs = wait_secs.min(86_400);
let deadline = std::time::Instant::now() + std::time::Duration::from_secs(wait_secs);
```

---

## P3-RB-2 — Hoist `unix_secs_i64()` helper for 5 cast sites

**Bundles:** RB-3, RB-10
**Effort:** ~10 min
**Sites:** `src/watch_status.rs:229`, `src/cli/batch/mod.rs:779`, `src/cli/batch/mod.rs:1988`, `src/cli/commands/infra/ping.rs:122`, `src/cli/watch/mod.rs:159`

**Fix:** Add to `src/lib.rs` (or `src/time.rs` if creating module):
```rust
/// Defensive `SystemTime::now() → Unix seconds as i64`. Returns `None` when
/// the clock is before epoch (RTC mis-set, hypervisor pause, etc.) and
/// emits a `tracing::warn!` once per process so journal surfaces bad-clock
/// conditions. Use everywhere instead of bare `as_secs() as i64`.
pub fn unix_secs_i64() -> Option<i64> {
    use std::sync::OnceLock;
    static WARNED: OnceLock<()> = OnceLock::new();
    match std::time::SystemTime::now().duration_since(std::time::SystemTime::UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_secs()).ok(),
        Err(_) => {
            WARNED.get_or_init(|| {
                tracing::warn!("system clock is before UNIX epoch — timestamps will be None");
            });
            None
        }
    }
}
```

Migrate the 5 callsites:
| Site | Current pattern |
|------|----------------|
| `watch_status.rs:226-231` (`now_unix_secs`) | replace body with `unix_secs_i64().unwrap_or(0)` — and consider changing return to `Option<i64>` per RB-10 (optional, slightly more invasive). |
| `cli/batch/mod.rs:779` | swap `.map(\|d\| d.as_secs() as i64)` chain for `cqs::unix_secs_i64()` |
| `cli/batch/mod.rs:1988` | same |
| `cli/commands/infra/ping.rs:122` | same |
| `cli/watch/mod.rs:159` (the `last_synced_at` line — uses metadata's modified time, NOT `SystemTime::now()`) | leave for a separate pass — different shape (`m.modified()` not `now()`); keep the `as_secs() as i64` but wrap in `i64::try_from(d.as_secs()).ok()` for overflow defense. |

---

## P3-RB-3 — `as_millis() as i64` truncation in reindex

**File:** `src/cli/watch/reindex.rs:507-508`
**Effort:** ~3 min
**Finding:** RB-4

**Fix:**
```rust
// before:
.map(|d| d.as_millis() as i64)

// after — surface overflow as None, treated same as missing mtime:
.and_then(|d| i64::try_from(d.as_millis()).ok())
```

---

## P3-RB-4 — Cap `migrate_legacy` sentinel read

**File:** `src/slot/mod.rs:656`
**Effort:** ~5 min
**Finding:** RB-5

**Fix:**
```rust
// before:
let detail = fs::read_to_string(&sentinel).unwrap_or_default();

// after — cap at 64 KiB matching SLOT_POINTER_MAX_BYTES sibling:
let detail = {
    use std::io::Read;
    let mut buf = String::new();
    fs::File::open(&sentinel)
        .and_then(|f| f.take(64 * 1024).read_to_string(&mut buf).map(|_| ()))
        .map(|_| buf)
        .unwrap_or_default()
};
```

---

## P3-RB-5 — `enumerate_files` strip_prefix mismatch warn-and-skip

**File:** `src/lib.rs:680-685`
**Effort:** ~5 min
**Finding:** RB-6

**Fix:**
```rust
// before:
if path.starts_with(&root) {
    Some(path.strip_prefix(&root).unwrap_or(&path).to_path_buf())
} else { /* ... */ }

// after — surface case-insensitive-fs disagreement:
match path.strip_prefix(&root) {
    Ok(rel) => Some(rel.to_path_buf()),
    Err(_) => {
        tracing::warn!(
            path = %path.display(),
            root = %root.display(),
            "starts_with said yes but strip_prefix failed — case-insensitive fs?"
        );
        None
    }
}
```

---

## P3-RB-6 — Saturating cast on WatchSnapshot counters

**File:** `src/watch_status.rs:213-218`
**Effort:** ~3 min
**Finding:** RB-7

**Fix:** Replace bare `as u64` casts with saturating conversions (defense-in-depth, matches RB-V1.30-3 pattern):
```rust
// before:
modified_files: input.pending_files_count as u64,
pending_notes: input.pending_notes,
rebuild_in_flight: input.rebuild_in_flight,
delta_saturated: input.delta_saturated,
incremental_count: input.incremental_count as u64,
dropped_this_cycle: input.dropped_this_cycle as u64,

// after — saturating:
modified_files: u64::try_from(input.pending_files_count).unwrap_or(u64::MAX),
// (others unchanged or apply same pattern to incremental_count + dropped_this_cycle)
incremental_count: u64::try_from(input.incremental_count).unwrap_or(u64::MAX),
dropped_this_cycle: u64::try_from(input.dropped_this_cycle).unwrap_or(u64::MAX),
```

---

## P3-RB-7 — `print_text_report` empty-fixture refusal

**File:** `src/cli/commands/eval/mod.rs:296-309`
**Effort:** ~5 min
**Finding:** RB-8

**Fix:** Short-circuit at the top of `print_text_report`:
```rust
if report.overall.n == 0 {
    eprintln!("[eval] no queries with gold_chunk; refusing to emit report (use --allow-empty to override)");
    std::process::exit(2);
}
```
Do this before computing `pct(...)`. Stops `NaN%` from leaking into reports and downstream gate checks.

---

## P3-SHL-1 — `wait_for_fresh` poll-interval env knob

**File:** `src/daemon_translate.rs:663`
**Effort:** ~5 min
**Finding:** SHL-V1.30-2

**Fix:** Add `CQS_FRESHNESS_POLL_MS` to `crate::limits` (default 250, floor 25, ceiling 5000). Read once per `wait_for_fresh` call (not cached) so tests can flip values.

---

## P3-SHL-2 — Drop `--require-fresh-secs` 600 s clamp (or warn on engagement)

**File:** `src/cli/commands/eval/mod.rs:237`
**Effort:** ~5 min
**Finding:** SHL-V1.30-3

**Fix (cheap):** Add `tracing::warn!` when the `min(600)` clamp engages:
```rust
let budget_secs = if wait_secs > 600 {
    tracing::warn!(requested = wait_secs, capped = 600u64,
        "--require-fresh-secs capped at 600 — set CQS_EVAL_REQUIRE_FRESH_MAX_SECS to override");
    600
} else { wait_secs };
```
Optionally read `CQS_EVAL_REQUIRE_FRESH_MAX_SECS` for an env override.

---

## P3-SHL-3 — Honor `CQS_GATHER_*` env vars in `task::run_task`

**File:** `src/task.rs:19-25, 143-149`
**Effort:** ~5 min
**Finding:** SHL-V1.30-4

**Fix:** Drop the `.with_max_expanded_nodes(TASK_GATHER_MAX_NODES)` override that masks `CQS_GATHER_MAX_NODES`. Either:
- **Option (a):** Remove the `.with_*` calls so gather's existing env-knob defaults flow through.
- **Option (b):** Rename to `CQS_TASK_GATHER_DEPTH` / `CQS_TASK_GATHER_MAX_NODES` env knobs that override the `TASK_*` constants per-task.

Option (a) is the cheaper one — the user's `CQS_GATHER_*` setting just works.

---

## P3-SHL-4 — Onboard caps env knobs

**File:** `src/onboard.rs:30-33, 174-175`
**Effort:** ~5 min
**Finding:** SHL-V1.30-5

**Fix:** Promote `MAX_CALLEE_FETCH = 30` and `MAX_CALLER_FETCH = 15` to env-overridable resolvers:
```rust
fn max_callee_fetch() -> usize {
    std::env::var("CQS_ONBOARD_CALLEE_FETCH")
        .ok().and_then(|v| v.parse().ok()).unwrap_or(30)
}
fn max_caller_fetch() -> usize {
    std::env::var("CQS_ONBOARD_CALLER_FETCH")
        .ok().and_then(|v| v.parse().ok()).unwrap_or(15)
}
```
Surface truncation in `OnboardSummary` JSON when caps engage so consumers see "I dropped N callers".

---

## P3-SHL-5 — `MAX_REFERENCES` env knob

**File:** `src/config.rs:390-405`
**Effort:** ~5 min
**Finding:** SHL-V1.30-6

**Fix:**
```rust
// before:
const MAX_REFERENCES: usize = 20;

// after — replace with crate::limits::max_references():
fn max_references() -> usize {
    std::env::var("CQS_MAX_REFERENCES")
        .ok().and_then(|v| v.parse().ok()).unwrap_or(20)
}
```
Update lines 391-404 to call `max_references()`. Memo the warning at load time so it doesn't fire on every `validate()`.

---

## P3-SHL-6 — Notes file-size + entry-count env knobs

**File:** `src/note.rs:20, 169, 245`
**Effort:** ~10 min
**Finding:** SHL-V1.30-7

**Fix:**
1. Hoist the duplicated `const MAX_NOTES_FILE_SIZE: u64 = 10 * 1024 * 1024;` (lines 169, 245) to module scope, single declaration.
2. Wrap with `crate::limits::max_notes_file_size()` reading `CQS_NOTES_MAX_FILE_SIZE` (default 10 MiB).
3. Same for `MAX_NOTES = 10_000` (line 20) → `CQS_NOTES_MAX_ENTRIES`.
4. Replace silent `.take(MAX_NOTES)` (line 331) with a `tracing::warn!` when truncation engages.

---

## P3-SHL-7 — `ENRICHMENT_PAGE_SIZE` env knob

**File:** `src/cli/enrichment.rs:46, 127`
**Effort:** ~3 min
**Finding:** SHL-V1.30-8

**Fix:**
```rust
fn enrichment_page_size() -> usize {
    std::env::var("CQS_ENRICHMENT_PAGE_SIZE")
        .ok().and_then(|v| v.parse().ok()).unwrap_or(500)
}
```
Replace `ENRICHMENT_PAGE_SIZE` const at lines 46, 127 with the resolver.

---

## P3-SHL-8 — `LAST_INDEXED_PRUNE_SIZE_THRESHOLD` env knob

**File:** `src/cli/watch/gc.rs:36-42`
**Effort:** ~3 min
**Finding:** SHL-V1.30-9

**Fix:** Replace const with `crate::limits` resolver reading `CQS_WATCH_PRUNE_SIZE_THRESHOLD` (default 5_000). Update doc to drop "intentionally not an env var" wording.

---

## P3-SHL-9 — Drop `OnceLock` cache on `daemon_periodic_gc_cap`

**File:** `src/cli/watch/gc.rs:78-86`
**Effort:** ~3 min
**Finding:** SHL-V1.30-10

**Fix:**
```rust
// before:
fn daemon_periodic_gc_cap() -> usize {
    static CACHE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *CACHE.get_or_init(|| {
        std::env::var("CQS_DAEMON_PERIODIC_GC_CAP")
            .ok().and_then(|v| v.parse().ok())
            .unwrap_or(DAEMON_PERIODIC_GC_CAP_DEFAULT)
    })
}

// after — read on every call so systemctl set-environment works:
fn daemon_periodic_gc_cap() -> usize {
    std::env::var("CQS_DAEMON_PERIODIC_GC_CAP")
        .ok().and_then(|v| v.parse().ok())
        .unwrap_or(DAEMON_PERIODIC_GC_CAP_DEFAULT)
}
```
One `getenv` per GC tick is microseconds; ticks are minutes apart. Matches `reconcile_enabled` semantic.

---

## P3-AC-1 — Case-fold `is_structural_query`

**File:** `src/search/router.rs:813-816`
**Effort:** ~5 min
**Finding:** AC-V1.30.1-2

**Fix:** Note that line 643 already passes `&query_lower`, so the bug is at *other* callers (lines 900, 1265, 1275-1276) that pass raw query strings. Either:
- **Option (a) (cleaner):** Lowercase inside `is_structural_query`:
  ```rust
  fn is_structural_query(query: &str) -> bool {
      let query_lower = query.to_ascii_lowercase();
      let query = query_lower.as_str();
      // ... rest unchanged ...
  }
  ```
- **Option (b):** Force every caller to pre-lowercase. (More fragile.)

Pick option (a). Add a regression-pin test: `assert!(is_structural_query("Class Foo"))`, `assert!(is_structural_query("Trait Iterator"))`, `assert!(is_structural_query("FIND ALL STRUCTS"))`.

---

## P3-AC-2 — `wait_for_fresh` deadline-first ordering

**File:** `src/daemon_translate.rs:660-679`
**Effort:** ~5 min
**Finding:** AC-V1.30.1-6

**Reframed during verification:** the budget overrun is up to ~5 s (one `daemon_status` read+write timeout, set at `daemon_translate.rs:444`), not 30 s. Mechanical fix unchanged.

**Fix:** Move the `if Instant::now() >= deadline` check *before* the `daemon_status` call so a stuck status RPC doesn't push the helper over budget by up to ~5 s (one `daemon_status` read+write timeout).
```rust
loop {
    if std::time::Instant::now() >= deadline {
        return FreshnessWait::Timeout(WatchSnapshot::unknown());
    }
    match daemon_status(cqs_dir) { ... }
    // ... rest unchanged
    std::thread::sleep(POLL_INTERVAL);
}
```

---

## P3-AC-3 — `BoundedScoreHeap::push` total_cmp on score equality

**File:** `src/search/scoring/candidate.rs:231`
**Effort:** ~3 min
**Finding:** AC-V1.30.1-7

**Fix:**
```rust
// before (line 231):
let better = score > *worst_score || (score == *worst_score && id < *worst_id);

// after — use total_cmp for consistency with the OrderedFloat wrapper:
use std::cmp::Ordering;
let better = match score.total_cmp(worst_score) {
    Ordering::Greater => true,
    Ordering::Equal => id < *worst_id,
    Ordering::Less => false,
};
```
Add a `debug_assert!(score.is_finite())` immediately above to pin the upstream filter invariant.

---

## P3-AC-4 — `idle_secs` sub-second resolution

**File:** `src/watch_status.rs:219`
**Effort:** ~3 min
**Finding:** AC-V1.30.1-8

**Fix:** Add `idle_ms: u64` field on `WatchSnapshot` populated from `last_event.elapsed().as_millis() as u64`. Keep `idle_secs` for backwards compat. Document the addition in the struct's wire-shape doc.

---

## P3-AC-5 — Skip: covered by P2

- **AC-V1.30.1-3** (BFS expansion-capped score-bump): **Covered by:** P2 entry of same ID.
- **AC-V1.30.1-9** (DefaultHasher socket name): **Covered by:** P2 entry of same ID.
- **AC-V1.30.1-10** (incremental_count idle-clear reset): **Covered by:** P2 entry of same ID.

---

## P3-EX-1 — `log_query` table-driven via dispatch macro

**File:** `src/cli/batch/commands.rs:508, 531, 567, 577, 581, 603`
**Effort:** ~10 min
**Finding:** EX-V1.30.1-3

**Fix:** Extend the existing `for_each_batch_cmd_pipeability!` macro (line 372-447) with a `log_as: Option<&str>` column. The macro emits the `log_query` call automatically when `log_as` is `Some(name)`. Removes 6 hand-sprinkled call sites. Normalize the arg-struct field name to `query` (or add a `fn query(&self) -> &str` accessor) so the macro can reach it without the per-variant divergence (`args.description` vs `args.query`).

---

## P3-EX-2 — Centralize CQS_* env-var falsy parsing

**File:** `src/cli/commands/eval/mod.rs:282-289` (callers across 30+ sites)
**Effort:** ~10 min
**Finding:** EX-V1.30.1-7

**Fix:** Add to `src/lib.rs` (or new `src/env.rs` module):
```rust
const FALSY: &[&str] = &["0", "false", "no", "off"];
const TRUTHY: &[&str] = &["1", "true", "yes", "on"];

pub fn env_truthy(name: &str) -> bool {
    std::env::var(name).map(|v| TRUTHY.contains(&v.trim().to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}
pub fn env_falsy(name: &str) -> bool {
    std::env::var(name).map(|v| FALSY.contains(&v.trim().to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}
```
Migrate `env_disables_freshness_gate()` to use `crate::env_falsy("CQS_EVAL_REQUIRE_FRESH")`. Audit can flag the 30+ other sites in a follow-up PR.

---

## P3-PB-1 — `--no-auth` warning uses `IpAddr::is_loopback()` (no false positives on 127.0.0.0/8)

**File:** `src/cli/commands/serve.rs:27`
**Effort:** ~5 min
**Finding:** PB-V1.30.1-1

**Reframed during verification:** the original "misses wildcard binds" framing is wrong — the existing predicate `bind != "127.0.0.1" && bind != "localhost" && bind != "::1"` *does* warn for `0.0.0.0` and `::`. The real defect is the opposite: it over-warns on the rest of `127.0.0.0/8` (e.g. `127.0.0.2`), which are loopback. Refactoring to `IpAddr::is_loopback()` is still cleaner and keeps wildcard coverage intact.

**Fix:** Replace the string-equality predicate with `IpAddr::is_loopback()`:
```rust
// before:
if no_auth && bind != "127.0.0.1" && bind != "localhost" && bind != "::1" {

// after — parse once, check is_loopback (suppresses warn for 127.0.0.0/8 and ::1 alike;
// still warns for 0.0.0.0, ::, and any LAN address):
let is_loopback = bind.parse::<std::net::IpAddr>()
    .map(|ip| ip.is_loopback())
    .unwrap_or(matches!(bind.as_str(), "localhost"));
if no_auth && !is_loopback {
```

(Combine with P3-CQ-3 above — drop the redundant duplicate and keep this single warning as the canonical surface.)

---

## P3-PB-2 — `--bind localhost` resolution

**File:** `src/cli/commands/serve.rs:39-41`
**Effort:** ~5 min
**Finding:** PB-V1.30.1-2

**Fix:** Resolve `localhost` to `127.0.0.1` before `parse::<SocketAddr>`:
```rust
let bind_str = if bind == "localhost" { "127.0.0.1" } else { bind.as_str() };
let bind_addr: SocketAddr = format!("{bind_str}:{port}")
    .parse()
    .with_context(|| format!("Failed to parse {bind_str}:{port} as a SocketAddr"))?;
```

---

## P3-PB-3 — Skip: P2 territory

- **PB-V1.30.1-3** (`tasklist INFO:` localized): **Covered by:** P2 entry of same ID.
- **PB-V1.30.1-7** (Windows hook fire): **Covered by:** P2 entry of same ID.
- **PB-V1.30.1-9** (reconcile path normalization): **Covered by:** P2 entry of same ID.

---

## P3-PB-4 — `atomic_replace` skip parent-dir fsync on Windows

**File:** `src/fs.rs:90-108`
**Effort:** ~3 min
**Finding:** PB-V1.30.1-6

**Fix:** Wrap the parent-fsync block in `#[cfg(unix)]`:
```rust
// before:
if let Some(parent) = final_path.parent() {
    match std::fs::File::open(parent) { ... }
}

// after:
#[cfg(unix)]
if let Some(parent) = final_path.parent() {
    match std::fs::File::open(parent) { ... }
}
```
The doc comment at line 85-89 already promises this no-op behavior. One syscall less per persisted file on Windows; debug-spam ends.

---

## P3-PB-5 — `git_dir` path normalization in hook reports

**File:** `src/cli/commands/infra/hook.rs:99-105, 152, 354`
**Effort:** ~10 min
**Finding:** PB-V1.30.1-8

**Fix:** Change `git_dir: PathBuf` → `git_dir: String` on `InstallReport`, `UninstallReport`, `StatusReport`, `FireReport`. Store `cqs::normalize_path(&path)` at construction. Same for `dirty_marker: Option<PathBuf>` → `Option<String>`. Match the convention in the rest of the JSON surface (per `src/store/types.rs:220`).

---

## P3-PB-6 — Linux daemon restart fallback when systemd unit missing

**File:** `src/cli/commands/infra/model.rs:710-738`
**Effort:** ~10 min
**Finding:** PB-V1.30.1-10

**Fix:** Probe `systemctl --user is-enabled cqs-watch` first. On exit code != 0 (unit not loaded), fall back to spawning `cqs watch --serve` directly — same pattern as the macOS branch at line 745.
```rust
let probe = std::process::Command::new("systemctl")
    .args(["--user", "is-enabled", "cqs-watch"])
    .output();
let unit_exists = matches!(probe, Ok(o) if o.status.success());
if unit_exists {
    // existing systemctl --user start path
} else {
    // spawn cqs watch --serve directly (mirror macOS branch)
}
```

---

## P3-SEC-1 — `cqs ref add` walk parents and chmod

**File:** `src/cli/commands/infra/reference.rs:137-145`
**Effort:** ~5 min
**Finding:** SEC-V1.30.1-9

**Fix:** Walk every parent the call may have created and chmod each. Or set process umask to `0o077` for the duration of `create_dir_all`:
```rust
#[cfg(unix)]
{
    use std::os::unix::fs::PermissionsExt;
    // ensure ~/.local/share/cqs/refs/ is also 0o700
    if let Some(refs_root) = ref_dir.parent() {
        let _ = std::fs::set_permissions(refs_root, std::fs::Permissions::from_mode(0o700));
    }
    // existing chmod on ref_dir itself
}
```
Mirror the SEC-D.6 socket pattern at `watch/mod.rs:496` if simpler.

---

## P3-SEC-2 — `cqs ref add` chmod 0o600 on index DB

**File:** `src/cli/commands/infra/reference.rs:165-178`
**Effort:** ~5 min
**Finding:** SEC-V1.30.1-10

**Fix:** After `Store::open(...)` succeeds, walk every file in `ref_dir` and chmod to `0o600` (Unix only). Match the pattern in `cqs export-model` for `model.toml`.
```rust
#[cfg(unix)]
{
    use std::os::unix::fs::PermissionsExt;
    for entry in std::fs::read_dir(&ref_dir).into_iter().flatten().flatten() {
        if let Ok(meta) = entry.metadata() {
            if meta.is_file() {
                let _ = std::fs::set_permissions(entry.path(), std::fs::Permissions::from_mode(0o600));
            }
        }
    }
}
```

---

## P3-SEC-3 — `escapeHtml` mirror in callgraph-3d.js

**File:** `src/serve/assets/views/callgraph-3d.js:55`
**Effort:** ~3 min
**Finding:** SEC-V1.30.1-3

**Fix:** Mirror the `escapeHtml` helper at the top of the file (matching `cluster-3d.js:21` / `hierarchy-3d.js:19`). Wrap `e.message` at line 55:
```js
// at top of file (after IIFE wrapper):
function escapeHtml(s) {
    return String(s).replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'})[c]);
}

// at line 55:
container.innerHTML = `<div class="error" style="margin:24px">3D bundle failed to load: ${escapeHtml(e.message)}</div>`;
```

---

## P3-SEC-4 — Skip: covered by P2

- **SEC-V1.30.1-4** (tag_user_code_trust_level shape coupling): **Covered by:** P2 entry of same ID.

---

## P3-PF-1 — `enumerate_files` skip-replacement-when-no-backslash

**File:** `src/cli/watch/reconcile.rs:99`
**Effort:** ~3 min
**Finding:** PF-V1.30.1-4

**Fix:**
```rust
// before:
let origin = rel.to_string_lossy().replace('\\', "/");

// after — Linux fast path stays Cow::Borrowed:
use std::borrow::Cow;
let origin_lossy = rel.to_string_lossy();
let origin: Cow<str> = if origin_lossy.contains('\\') {
    Cow::Owned(origin_lossy.replace('\\', "/"))
} else {
    origin_lossy
};
```
Use `origin.as_ref()` against the `HashMap<String, _>`. Cuts unnecessary allocations on Linux/WSL.

---

## P3-PF-2 — `build_stats` collapse 4 round-trips into 1

**File:** `src/serve/data.rs:1105-1128`
**Effort:** ~5 min
**Finding:** PF-V1.30.1-5

**Fix:**
```rust
let row: (i64, i64, i64, i64) = sqlx::query_as(
    "SELECT
        (SELECT COUNT(*) FROM chunks),
        (SELECT COUNT(DISTINCT origin) FROM chunks),
        (SELECT COUNT(*) FROM function_calls),
        (SELECT COUNT(*) FROM type_edges)"
).fetch_one(&store.pool).await?;
Ok(StatsResponse {
    total_chunks: row.0.max(0) as u64,
    total_files: row.1.max(0) as u64,
    call_edges: row.2.max(0) as u64,
    type_edges: row.3.max(0) as u64,
})
```

---

## P3-PF-3 — Pre-build cookie needle in AuthMiddlewareState

**File:** `src/serve/auth.rs:357, 292`
**Effort:** ~10 min
**Finding:** PF-V1.30.1-6 (also subsumes RM-6, RM-7)

**Fix:** Add `cookie_name: Arc<str>` and `cookie_lookup_needle: Arc<str>` to `AuthMiddlewareState`. Populate at construction time from `cookie_name_for_port(port)` and `format!("{cookie_name}=")`. Update `check_request` to take `&str` for the needle (or borrow from state directly). Both are `Arc<str>` so `Clone` of the state stays cheap. Net: zero allocations per request for auth happy path.

---

## P3-PF-4 — Watch reindex content_hash clone reduction

**File:** `src/cli/watch/reindex.rs:414-417`
**Effort:** ~5 min
**Finding:** PF-V1.30.1-7

**Fix (cheap option):** Pre-allocate `Vec::with_capacity(to_embed.len())` to avoid resize cost. Real fix: change downstream HNSW insert API to take `&[&str]` so the clone disappears entirely. Pre-allocate as the immediate win:
```rust
let mut content_hashes: Vec<String> = Vec::with_capacity(to_embed.len());
content_hashes.extend(to_embed.iter().map(|(_, c)| c.content_hash.clone()));
```

---

## P3-PF-5 — Cache `last_synced_at` to skip `fs::metadata` syscall

**File:** `src/cli/watch/mod.rs:149-185, 1303`
**Effort:** ~10 min
**Finding:** PF-V1.30.1-1

**Fix (cheap):** Throttle the metadata call to once per N ticks (e.g., every 10s) since `last_synced_at` is whole-second resolution anyway. Use a `last_metadata_check: Instant` field on `WatchState`.

**Fix (proper):** Add `last_synced_at: Arc<AtomicI64>` to `WatchState`, updated only when the daemon successfully commits a write batch. Publish path reads atomic with no syscall. Strictly better — zero stat() syscalls and exact precision.

---

## P3-RM-1 — Drop `thread_local! REQ_LINE` (premise was wrong)

**File:** `src/cli/watch/socket.rs:91-99`
**Effort:** ~3 min
**Finding:** RM-1

**Fix:** Daemon spawns a fresh thread per accept (`daemon.rs:189-205`), not a Tokio blocking pool — so the thread_local doesn't amortize anything. Replace with a plain `let mut line = String::with_capacity(8192);` at the call site. Drop the `thread_local!` block. Same cost, simpler code, comment stops lying.

---

## P3-RM-2 — `read_context_lines` bounded read

**File:** `src/cli/display.rs:59-99, 489`
**Effort:** ~10 min
**Finding:** RM-3

**Reframed during verification:** function name corrected — production fn is `read_context_lines` (`display.rs:16-100`); the test-only mirror is `read_context_lines_test` (`display.rs:483-519`). No `compute_context` exists. Both have the `read_to_string(file)` + `content.lines().collect()` pattern at the cited lines (59 and 489 respectively).

**Fix:** Replace `std::fs::read_to_string(file)` + `content.lines().collect()` with a `BufReader` that breaks early. The bound has to be computed up front from the input args (`line_end + context + 1`) since the indexing logic below currently relies on having `lines.len()` for clamping:
```rust
use std::io::{BufRead, BufReader};
let f = std::fs::File::open(file).with_context(...)?;
// line_start/line_end are already normalised above (max(1)); compute upper bound
// before clamping so we don't pull lines we'll discard.
let limit = (line_end as usize)
    .saturating_add(context)
    .saturating_add(1);
let lines: Vec<String> = BufReader::new(f)
    .lines()
    .take(limit)
    .map(|l| l.unwrap_or_default().trim_end_matches('\r').to_string())
    .collect();
// rest of indexing logic unchanged
```
Apply at both `read_context_lines` (`display.rs:59`) and `read_context_lines_test` (`display.rs:489`).

---

## P3-RM-3 — Skip: P4 / covered

- **RM-2** (wait_for_fresh socket churn): **Covered by:** RB-9 + P4 PF-V1.30.1-2.
- **RM-4** (HNSW snapshot map): **medium**, P4 territory.
- **RM-5** (reconcile holds full repo set): **medium**, P4 territory.
- **RM-6, RM-7**: **Covered by:** P3-PF-3 above.

---

## P3-TC-HAP-1 — Add 4 missing happy-path tests

**Bundles:** TC-HAP-1.30.1-1, TC-HAP-1.30.1-8, TC-HAP-1.30.1-9, TC-HAP-1.30.1-10
**Effort:** ~30 min total

**Reframed during verification:** signatures fixed against source — `daemon_reconcile(cqs_dir: &Path, hook: Option<&str>, args: &[String])` (not `&str` + `Vec<String>`); `print_text_report(report: &EvalReport)` writes to stdout via `println!` (no `Write` sink), so the -9 test needs a prerequisite refactor; `do_install` doesn't exist (only `cmd_install(no_overwrite, json)` + the lower-level `write_hook_script`), so the -1 test should drive `write_hook_script` directly the way the existing `install_writes_three_hooks_into_fresh_repo` test (`hook.rs:441-456`) does, OR a separate prep prompt should extract `do_install` first. `let mut inp` in -8 is unnecessary — `compute(input(...))` consumes by value.

**TC-HAP-1.30.1-1 — `cmd_install` upgrade-marker** (`src/cli/commands/infra/hook.rs::tests`).

There are two ways to land this; pick one:

(a) **Drive the existing helpers directly** — matches `install_writes_three_hooks_into_fresh_repo` (`hook.rs:441-456`). No new public surface required.

```rust
#[test]
fn install_upgrade_replaces_v0_marker_with_current() {
    let tmp = tempfile::tempdir().unwrap();
    let hooks = tmp.path().join(".git").join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    let path = hooks.join("post-checkout");
    std::fs::write(&path, "#!/bin/sh\n# cqs:hook v0\n").unwrap();
    // Pre-check: marker is the legacy prefix, not current.
    let pre = std::fs::read_to_string(&path).unwrap();
    assert!(pre.contains(HOOK_MARKER_PREFIX));
    assert!(!pre.contains(HOOK_MARKER_CURRENT));
    // Drive the lower-level write directly (cmd_install would call
    // find_project_root, which is bound to the workspace, not the temp tree).
    write_hook_script(&path, "post-checkout").unwrap();
    let body = std::fs::read_to_string(&path).unwrap();
    assert!(body.contains(HOOK_MARKER_CURRENT));
}

#[test]
fn install_idempotent_second_run_keeps_marker() {
    // After two write_hook_script(...) calls, file still contains HOOK_MARKER_CURRENT once.
}

#[test]
fn install_no_overwrite_path_skips_when_hook_absent() {
    // Reproduce the cmd_install None branch with no_overwrite=true: skipped_no_overwrite gets the hook;
    // assert the file was NOT written.
}
```

(b) **Prerequisite refactor first** — split this prompt into:
   - **TC-HAP-1.30.1-1a (refactor):** extract `fn do_install(git_dir: &Path, no_overwrite: bool) -> Result<InstallReport>` from `cmd_install` (`hook.rs:149`). `cmd_install` becomes a thin wrapper that calls `find_project_root()` + `locate_git_hooks_dir()` + `do_install`.
   - **TC-HAP-1.30.1-1b (test):** then the original test skeleton works as written:
     ```rust
     let report = do_install(&hooks, false).unwrap();
     assert_eq!(report.upgraded.len(), 1);
     ```

Pick (a) if minimising scope; (b) if a `do_install` is wanted for other tests too.

**TC-HAP-1.30.1-8 — `WatchSnapshot::compute` Rebuilding state** (`src/watch_status.rs::tests`).

Note: existing `rebuild_dominates_over_stale_files` (`watch_status.rs:278`) covers rebuild + queued files. This adds the zero-pending case:

```rust
#[test]
fn compute_with_rebuild_in_flight_zero_pending_returns_rebuilding() {
    let snap = WatchSnapshot::compute(input(0, true, false, 0));  // helper at :237
    assert_eq!(snap.state, FreshnessState::Rebuilding);
    assert!(!snap.is_fresh());
    assert_eq!(snap.modified_files, 0);
}
```

(Drop the `let mut inp = ...` — `compute` consumes the value; binding it adds nothing.)

**TC-HAP-1.30.1-9 — `print_text_report` canonical format** (`src/cli/commands/eval/mod.rs::tests`).

Current signature: `fn print_text_report(report: &EvalReport)` — writes via `println!` to stdout. Two paths:

(a) **Prerequisite refactor:** change to `fn print_text_report<W: std::io::Write>(report: &EvalReport, w: &mut W) -> std::io::Result<()>` (or `&mut dyn Write`). Update the one caller. Then test against a `Vec<u8>` sink:
```rust
#[test]
fn print_text_report_renders_canonical_header_and_metrics() {
    let report = EvalReport { /* deterministic fixture */ };
    let mut buf = Vec::new();
    print_text_report(&report, &mut buf).unwrap();
    let out = String::from_utf8(buf).unwrap();
    assert!(out.contains("=== eval results: test (N=2) ==="));
    assert!(out.contains("R@1=50%"));   // current pct() format — adjust if pct returns "0.5"
    assert!(out.contains("R@5=100%"));
}
```

(b) **No-refactor fallback:** drop this from the bundle and re-file as a `print_text_report` `&mut dyn Write` refactor proposal. Capturing stdout under `cargo test` is brittle (parallel tests, threading); it's not worth the test if the function isn't refactored.

**TC-HAP-1.30.1-10 — `daemon_reconcile` forwards args verbatim** (`src/daemon_translate.rs::tests`).

Real signature (verified `daemon_translate.rs:537-541`):
```rust
pub fn daemon_reconcile(
    cqs_dir: &std::path::Path,
    hook: Option<&str>,
    args: &[String],
) -> Result<DaemonReconcileResponse, String>
```

```rust
#[test]
fn daemon_reconcile_forwards_hook_args_verbatim() {
    // extend the existing mock to *capture* the request line as a String
    let captured = daemon_reconcile(
        cqs_dir,
        Some("post-checkout"),
        &["abc123".into(), "def456".into(), "1".into()],
    );
    // assert captured JSON: parsed_request["args"] == ["abc123","def456","1"]
}

#[test]
fn daemon_reconcile_forwards_unicode_args() {
    let _ = daemon_reconcile(
        cqs_dir,
        Some("post-merge"),
        &["mañana".into(), "🚀".into()],
    );
    // assert captured payload preserves UTF-8
}
```

---

## P3-TC-HAP-2 — Skip: P2 / hard / subsumed

- **TC-HAP-1.30.1-2,3,4,7** are P2 entries (hook commands, status, gate end-to-end). Skipped here.
- **TC-HAP-1.30.1-5** (Stale → Fresh transition): **medium** effort, falls into the P2 batch refactor of `wait_for_fresh` (cross-cutting bundle).
- **TC-HAP-1.30.1-6** (process_file_changes direct tests): **hard**, P4 territory.

---

# Summary

**Distinct prompts after grouping: 51 actionable + 9 explicit skip markers = 60 entries total**

78 raw P3 findings collapsed into 51 distinct actionable fix prompts. The 9 skip markers document why specific P3 IDs are covered by higher-priority fixes (P1/P2) or subsumed by another P3 bundle.

**Top 5 patterns bundled:**

1. **Per-file env-knob promotion** (P3-SHL-1 through P3-SHL-9, 9 sites) — replace hardcoded constants with `crate::limits::*` resolvers reading `CQS_*` env vars. Single canonical pattern, applied to wait poll interval, eval cap, task gather, onboard caps, MAX_REFERENCES, MAX_NOTES file/entries, ENRICHMENT_PAGE_SIZE, prune threshold, gc_cap caching.

2. **Documentation drift** (P3-DOC-1, 5 sites in one prompt) — README command list, CONTRIBUTING architecture overview, PRIVACY/SECURITY platform paths, README watch-mode default, CHANGELOG/ROADMAP cache subcommand list. All small text tweaks with no trust-claim shifts.

3. **`unwrap_or_default()`/`let _ =` swallow-error** (P3-EH-1, 4 sites in one prompt) — replace with explicit `match` + `tracing::warn!` per the post-v0.12.1 audit rule. Covers slot resolution, doctor list_slots, index --json envelope, reranker checksum write.

4. **Saturating/defensive timestamp casts** (P3-RB-2, P3-RB-3, P3-RB-6 — 3 prompts; ~7 cast sites total) — hoist `unix_secs_i64()` helper, swap `as i64` for `try_from(...).ok()`, use saturating cast on WatchSnapshot counters.

5. **Tracing observability papercuts** (P3-OB-1 through P3-OB-5, 5 prompts) — demote per-search info spam to debug; add closing events to wait_for_fresh; add reason-enum to 401 warns; add entry span + outcome event to require_fresh_gate; add elapsed_ms to reconcile/GC walks.

---

## P3 Verification Report

Verification pass run 2026-04-28 against source at HEAD (c19a2eef post-v1.30.1 + #1197 / #1198). Each prompt's "before" / cited line is checked; tableized prompts are spot-checked across all rows.

### P3-DOC-1: NEEDS FIX
**Issue (item 1 README:540-569):** the `cqs hook` and `cqs status --watch-fresh` rows are NOT yet in the canonical command list at README:540-569 — verified by grep; absent. Prompt's instruction to add them is correct, anchor is right.
**Issue (item 3 PRIVACY anchors):** Prompt cites "PRIVACY.md:21-22" as the `~/.cache/cqs/` line. Actual PRIVACY.md:20 is `~/.cache/cqs/embeddings.db`, :21 is `query_cache.db`, :22 is `query_log.jsonl` — three lines, not one. The fix needs to extend the legacy block at 18-22 with platform paths, not replace a single line.
**Issue (item 4 README:219-220):** existing line 219 already says "(250 ms poll, capped at 600 s)". Replacement adds default-30s; current text is acceptable but the "default 30 s budget" is more discoverable. Minor.
**Issue (item 5 ROADMAP/CHANGELOG):** verified both ROADMAP.md:131 and CHANGELOG.md:71 have `{stats,prune,compact}` (missing `clear`). Replacement to `{stats,clear,prune,compact}` is correct.
**Correction:** Item 1 anchors verified — insert two new rows after README:568. Item 3: cite anchors as PRIVACY.md:18-22 (legacy block) and SECURITY.md:111-115 (read-access table). Item 4 verbatim acceptable. Item 5 verbatim correct.

### P3-DOC-2: VERIFIED
Skip marker for DOC-V1.30.1-8 — folded into DOC-1 item 2.

### P3-CQ-1: VERIFIED
Lines 248-255 match exactly. `snap.modified_files`, `snap.dropped_this_cycle`, `snap.delta_saturated` all real fields on `WatchSnapshot`.

### P3-CQ-2: VERIFIED
`embedder/provider.rs:14`, `reranker.rs:100`, `splade/mod.rs:27` all hold the duplicate `ort_err`. The cross-module rationale comment at `reranker.rs:99` is verified.

### P3-CQ-3: VERIFIED
`cli/commands/serve.rs:27-37` matches; `serve/mod.rs:162-165` has the unconditional `WARN: --no-auth` warning.

### P3-API-1: NEEDS FIX
**Issue (API-V1.30.1-7):** Prompt cites `cli/watch/mod.rs:1303` as the WatchSnapshotInput caller; the real call site is at `cli/watch/mod.rs:165`. Line 1303 is the `publish_watch_snapshot(...)` invocation, not the struct construction.
**Correction:** Change "Migrate the one caller in `cli/watch/mod.rs:1303`" → "Migrate the one caller in `cli/watch/mod.rs:165`".
**All other rows verified:** API-V1.30.1-2 (definitions.rs:792 + eval/mod.rs:85), -3 (eval/mod.rs:79-80), -4 (daemon_translate.rs:236, watch_status.rs:105), -6 (daemon_translate.rs:517-519), -8 (status.rs:41-54), -9 (watch_status.rs:51-60), -10 (watch_status.rs:101 + 219).

### P3-API-2: VERIFIED
Skip marker for API-V1.30.1-5 (folded into bundle-wait-fresh).

### P3-EH-1: VERIFIED
All four sites verified at the cited lines: `dispatch.rs:207`, `doctor.rs:923`, `build.rs:863-867`, `reranker.rs:524`.

### P3-OB-1: VERIFIED
`router.rs:469-474, 491-496, 549-554, 1146-1150` all hold `tracing::info!` with the cited shapes.

### P3-OB-2: VERIFIED
`daemon_translate.rs:660-679` is the function body. Three terminal returns at lines 669, 672, 676 — all need the closing event per the prompt.

### P3-OB-3: VERIFIED
`auth.rs:389-401` is the warn site; `auth.rs:269-321` is `check_request`. Both line ranges correct. `AuthOutcome::Unauthorized` is bare today (no reason carried).

### P3-OB-4: VERIFIED
`eval/mod.rs:219-275` is the `require_fresh_gate` body. Three terminal paths (Fresh Ok, Timeout bail, NoDaemon bail) all need the outcome event.

### P3-OB-5: VERIFIED
`reconcile.rs:63-148` is `run_daemon_reconcile`; `gc.rs:103-180` is `run_daemon_startup_gc`; `gc.rs:195-243` is `run_daemon_periodic_gc`. All have terminal `tracing::info!` per the prompt.

### P3-OB-6: VERIFIED
Skip markers — OB-V1.30.1-8/-9/-10 covered by P2 entries.

### P3-TC-ADV-1: NEEDS FIX
**Issue (unwrap_dispatch_payload test):** Prompt's test calls `unwrap_dispatch_payload(v, "TestType")` but the function signature is `fn unwrap_dispatch_payload(output: &serde_json::Value, type_name: &str) -> Result<serde_json::Value, String>`. The first arg must be a reference: `&v`.
**Correction:** `let result = unwrap_dispatch_payload(&v, "TestType");` (& added).
**Other tests verified:** `try_from_string_accepts_long_alphabet_input_today` (AuthToken::try_from_string takes `impl Into<String>`); cookie/bearer pin tests pin current behavior accurately per `auth.rs:269-321`.

### P3-TC-ADV-2: VERIFIED
`eval/mod.rs:282-290` is `env_disables_freshness_gate`; `:405-432` is the test inline-rewriting the body. The rewrite pattern using `serial_test::serial(...)` matches existing convention in `daemon_translate.rs::tests`.

### P3-TC-ADV-3: VERIFIED
Skip marker for TC-ADV-1.30.1-8 — covered by CQ-V1.30.1-2 P1 fix.

### P3-RB-1: VERIFIED
`daemon_translate.rs:660-662` matches the function signature + deadline calc. The defensive `.min(86_400)` cap is sound.

### P3-RB-2: VERIFIED
All 5 sites verified: `watch_status.rs:226-231` (now_unix_secs), `cli/batch/mod.rs:779`, `cli/batch/mod.rs:1988`, `cli/commands/infra/ping.rs:122`, `cli/watch/mod.rs:159`. The 5th-site caveat about `m.modified()` vs `now()` is correct.

### P3-RB-3: VERIFIED
`cli/watch/reindex.rs:507-508` has `.map(|d| d.as_millis() as i64)`.

### P3-RB-4: VERIFIED
`slot/mod.rs:656` has `let detail = fs::read_to_string(&sentinel).unwrap_or_default();`.

### P3-RB-5: VERIFIED
`lib.rs:680-685` matches the `if path.starts_with(&root) { ... } else { ... }` pattern.

### P3-RB-6: VERIFIED
`watch_status.rs:213-218` matches the cited cast block.

### P3-RB-7: VERIFIED
`eval/mod.rs:296-309` is `print_text_report` body; the empty-fixture short-circuit before `pct(...)` is correct.

### P3-SHL-1: VERIFIED
`daemon_translate.rs:663` has the `poll_interval = Duration::from_millis(250)`.

### P3-SHL-2: VERIFIED
`eval/mod.rs:237` has `let budget_secs = wait_secs.min(600);`.

### P3-SHL-3: VERIFIED
`task.rs:19-25` (constants) and `task.rs:143-149` (`.with_max_expanded_nodes(...)` chain) both verified.

### P3-SHL-4: VERIFIED
`onboard.rs:30-33` (constants) and `:174-175` (caps) both verified.

### P3-SHL-5: VERIFIED
`config.rs:390-405` has `const MAX_REFERENCES: usize = 20;` and the validate truncate block.

### P3-SHL-6: VERIFIED
`note.rs:20` has `MAX_NOTES = 10_000`; lines 169 and 245 each have the duplicated `MAX_NOTES_FILE_SIZE`; `note.rs:331` has the silent `.take(MAX_NOTES)`.

### P3-SHL-7: VERIFIED
`enrichment.rs:46` has `const ENRICHMENT_PAGE_SIZE: usize = 500;`; `:127` has `chunks_paged(cursor, ENRICHMENT_PAGE_SIZE)`.

### P3-SHL-8: VERIFIED
`gc.rs:42` has `LAST_INDEXED_PRUNE_SIZE_THRESHOLD: usize = 5_000;`.

### P3-SHL-9: VERIFIED
`gc.rs:78-86` matches the `OnceLock` cache pattern.

### P3-AC-1: VERIFIED
`router.rs:813-816` is inside `is_structural_query`; the callers at 643 (lowercased), 900, 1265, 1275-1276 (raw) are correctly identified.

### P3-AC-2: NEEDS FIX
**Issue:** Prompt says "stuck status RPC doesn't push the helper over budget by up to one daemon-timeout's worth (~30s)". Actual `daemon_status` timeout is 5s (`daemon_translate.rs:444`), not 30s. The fix is still valid but the impact framing is inflated.
**Correction:** Update prompt to say "by up to ~5s (one daemon_status read+write timeout)" instead of ~30s. Mechanical fix unchanged.

### P3-AC-3: VERIFIED
`search/scoring/candidate.rs:231` has the `score > *worst_score || (score == *worst_score && id < *worst_id)` predicate.

### P3-AC-4: VERIFIED
`watch_status.rs:219` has `idle_secs: input.last_event.elapsed().as_secs()`. Adding `idle_ms` field is non-invasive.

### P3-AC-5: VERIFIED
Skip markers — AC-V1.30.1-3, -9, -10 covered by P2 entries.

### P3-EX-1: VERIFIED
All 6 `log_query` sites confirmed at `commands.rs:508, 531, 567, 577, 581, 603`. Macro `for_each_batch_cmd_pipeability!` at `:372-447` exists and is the right extension target.

### P3-EX-2: VERIFIED
`eval/mod.rs:282-289` matches the `env_disables_freshness_gate` body.

### P3-PB-1: NEEDS FIX
**Issue:** Premise is partially mistaken. Current code `bind != "127.0.0.1" && bind != "localhost" && bind != "::1"` DOES warn for `0.0.0.0` and `::` because they don't match any of the three exclusions. So the prompt's claim that the warning "misses wildcard binds" is wrong — it actually warns for them today. The `is_loopback()` refactor is still cleaner (catches `127.0.0.2` etc. as loopback, currently false-positive-warned), but the framing should be flipped.
**Correction:** Reword prompt: "Replace string-equality with `IpAddr::is_loopback()` so any 127.0.0.0/8 bind correctly suppresses the warning. Current code over-warns on `127.0.0.2` and similar — does NOT under-warn on `0.0.0.0`."

### P3-PB-2: VERIFIED
`serve.rs:39-41` matches.

### P3-PB-3: VERIFIED
Skip markers for PB-V1.30.1-3, -7, -9.

### P3-PB-4: VERIFIED
`fs.rs:90-108` has the parent-fsync block (open + sync_all). Wrapping in `#[cfg(unix)]` is correct.

### P3-PB-5: VERIFIED
`hook.rs:99-105` has `InstallReport { git_dir: PathBuf, ... }`; `:152` is `let git_dir = locate_git_hooks_dir(&root)?;`; `:354` is `let path = git_dir.join(hook);`.

### P3-PB-6: VERIFIED
`model.rs:710-738` has the systemctl restart block; `:745+` has the macOS `current_exe()` direct-spawn fallback.

### P3-SEC-1: VERIFIED
`reference.rs:137-145` has create_dir_all + chmod block.

### P3-SEC-2: VERIFIED
`reference.rs:165-178` is the Store::open + run_index_pipeline path. The fix to walk read_dir + chmod after is correct.

### P3-SEC-3: VERIFIED
`callgraph-3d.js:55` has the unescaped `${e.message}`. `cluster-3d.js:21` and `hierarchy-3d.js:19` both have the `escapeHtml` helper to mirror.

### P3-SEC-4: VERIFIED
Skip marker for SEC-V1.30.1-4.

### P3-PF-1: VERIFIED
`cli/watch/reconcile.rs:99` has `let origin = rel.to_string_lossy().replace('\\', "/");`.

### P3-PF-2: VERIFIED
`serve/data.rs:1105-1128` has the 4-query `build_stats` body matching the "before".

### P3-PF-3: VERIFIED
`serve/auth.rs:357` has the per-request `cookie_name_for_port(state.cookie_port)` allocation; `:292` has `let needle = format!("{cookie_name}=");`. The pre-build target `AuthMiddlewareState` at `:339-342` has `token: AuthToken, cookie_port: u16` fields.

### P3-PF-4: VERIFIED
`reindex.rs:414-417` is the `Vec::collect()` of cloned content_hashes.

### P3-PF-5: VERIFIED
`cli/watch/mod.rs:149-185` is `publish_watch_snapshot`; `:1303` is its only caller. The metadata fast-path optimization is mechanically sound.

### P3-RM-1: VERIFIED
`socket.rs:91-99` has `thread_local! { static REQ_LINE: RefCell<String> ... }`. `daemon.rs:189-205` confirms a fresh thread per accept (`std::thread::Builder::new().spawn(...)`), so the thread_local doesn't amortize across connections.

### P3-RM-2: NEEDS FIX
**Issue:** Prompt says fix targets `compute_context` at `display.rs:59` and `:489`. **No `compute_context` function exists in display.rs.** The actual functions are `read_context_lines` (`display.rs:16-100`) and `read_context_lines_test` (`display.rs:483+`, inside `#[cfg(test)] mod tests`). Both have the `read_to_string` + `lines().collect()` pattern at the cited line numbers (59, 489).
**Correction:** Rename target in prompt: "Replace the `read_to_string(file)` + `content.lines().collect()` pattern in `read_context_lines` (`display.rs:59`) and `read_context_lines_test` (`display.rs:489`)."

### P3-RM-3: VERIFIED
Skip markers for RM-2, RM-4, RM-5, RM-6, RM-7.

### P3-TC-HAP-1: NEEDS FIX
**Issue (TC-HAP-1.30.1-1):** Test skeleton uses `do_install(&hooks, false)` — **`do_install` does not exist in `hook.rs`**. Only `cmd_install(no_overwrite, json)` exists, which calls `find_project_root()` + `locate_git_hooks_dir()`. The prompt acknowledges in passing ("factor out a do_install...") but the test as-written won't compile.
**Issue (TC-HAP-1.30.1-10):** Test skeleton calls `daemon_reconcile(cqs_dir, "post-checkout", vec!["abc123".into(), ...])`. Real signature is `pub fn daemon_reconcile(cqs_dir: &std::path::Path, hook: Option<&str>, args: &[String]) -> Result<DaemonReconcileResponse, String>`. The hook arg must be `Some("post-checkout")` and args must be `&["abc123".into(), ...]` (slice ref, not Vec).
**Issue (TC-HAP-1.30.1-8):** Skeleton's `let mut inp = input(0, true, false, 0)` — the `mut` is unnecessary (compute moves the value). Also note the existing test `rebuild_dominates_over_stale_files` at `watch_status.rs:278-284` already covers `Rebuilding` state with files queued; this test is incremental (rebuild + zero pending).
**Issue (TC-HAP-1.30.1-9):** Test for `print_text_report` — function signature is `fn print_text_report(report: &EvalReport)` (prints to stdout via `println!`, not a `Write`-trait sink). Skeleton needs `print_text_report` refactored first to take `&mut dyn Write`.
**Correction:** TC-HAP-1.30.1-1: prerequisite step — refactor `cmd_install` to extract `do_install(git_dir: &Path, no_overwrite: bool) -> Result<InstallReport>` first; existing tests already follow the "drive the lower-level write directly" pattern (`hook.rs:441-456`, `install_writes_three_hooks_into_fresh_repo`). TC-HAP-1.30.1-10: fix sig — `daemon_reconcile(cqs_dir, Some("post-checkout"), &["abc123".into(), "def456".into(), "1".into()])`. TC-HAP-1.30.1-9: requires refactoring `print_text_report(report: &EvalReport, w: &mut dyn Write)` first.

### P3-TC-HAP-2: VERIFIED
Skip markers for TC-HAP-1.30.1-2, -3, -4, -5, -6, -7.

---

### P3 Verification Summary

- **VERIFIED:** 41
- **NEEDS FIX:** 8 (DOC-1, API-1, TC-ADV-1, AC-2, PB-1, RM-2, TC-HAP-1, plus minor anchor inaccuracies in DOC-1)
- **ALREADY FIXED:** 0

### Top Systematic Defects Across NEEDS FIX

1. **Wrong function names / line numbers from drift** — P3-RM-2 cites `compute_context` (doesn't exist; function is `read_context_lines`); P3-API-1 cites `cli/watch/mod.rs:1303` for WatchSnapshotInput caller (real line 165, line 1303 is `publish_watch_snapshot` invocation). Both stem from the prompt author searching by function role rather than reading source.
2. **Fictional helpers required by test skeletons** — P3-TC-HAP-1's `do_install` doesn't exist; the prompt assumes a refactor that hasn't happened. P3-TC-HAP-1's `print_text_report` test assumes a Write-trait refactor. These prompts mix prerequisite refactor with the test it enables.
3. **Wrong API signatures in test skeletons** — P3-TC-HAP-1's `daemon_reconcile` call passes `&str` for `Option<&str>` and `Vec<String>` for `&[String]`. P3-TC-ADV-1's `unwrap_dispatch_payload(v, ...)` passes value where `&serde_json::Value` is expected.
4. **Inflated impact framing** — P3-AC-2 says "~30s overrun" when daemon_status timeout is actually 5s. P3-PB-1 says wildcard binds are "missed" when current code DOES warn for them. Mechanical fixes still valid; triage rationale overstated.

### P3 Promotions Suggested

None — no NEEDS FIX entry surfaces a hidden P1. Defects are mechanical (line drift, wrong sig, fictional helper) rather than impact mismeasurement. AC-2 and PB-1 inflated impact still keeps them P3 once the framing is corrected.

# P4 Trivial Inline Fixes

## P4-trivial: SEC-V1.30.1-5 — `trust_level: "user-code"` covers vendored third-party in tree
**File:** `SECURITY.md` (next to existing trust-level discussion) + `src/store/helpers/types.rs:172-196`
**Effort:** ~5 min
**Fix:** Per the audit's "option (a)" suggested fix — add a one-paragraph note to `SECURITY.md` clarifying that `trust_level: "user-code"` means *"from the user's project store"* (i.e., not from a `cqs ref` reference index), not *"authored by the user"*. Vendored upstream content (`vendor/`, `third_party/`, `node_modules/`, copied SDKs committed to the project tree) and content surfaced by `cqs notes mention` from `docs/notes.toml` retain `user-code` even though they are exactly the indirect-prompt-injection surface the trust-level field exists to flag. The proper fix (path-prefix denylist + per-chunk `vendored: bool`) is captured under P4-issue. Doc-only clarification, no code change. Pairs with the SEC-V1.30.1-1 / SEC-V1.30.1-2 lying-docs cluster already in P1.

## P4-trivial: DS-V1.30.1-D6 — duplicate of CQ-V1.30.1-2 (P1)
**File:** `src/watch_status.rs:199-209`
**Effort:** 0 min — no separate fix
**Fix:** Already covered by the P1 fix for CQ-V1.30.1-2 (`compute()` ignores `delta_saturated`). The triage table flags this as a duplicate so it doesn't get re-implemented; reference the P1 PR's commit when closing the audit pass.

## P4-trivial: DS-V1.30.1-D8 — duplicate of CQ-V1.30.1-1 (P1)
**File:** `src/cli/watch/events.rs:139-146`
**Effort:** 0 min — no separate fix
**Fix:** Already covered by the P1 fix for CQ-V1.30.1-1 (`dropped_this_cycle` reset before publish). Same duplicate-cross-reference pattern as DS-V1.30.1-D6.

## P4-trivial: DOC-V1.30.1-8 — subsumed by DOC-V1.30.1-3 (P1)
**File:** `CONTRIBUTING.md`
**Effort:** 0 min — no separate fix
**Fix:** Folded into the DOC-V1.30.1-3 P1 fix (CONTRIBUTING Architecture Overview refresh). When that PR lands, this entry is closed automatically.

## P4-trivial: PF-V1.30.1-2 — covered by RB-9 + RM-2
**File:** `src/daemon_translate.rs:660-679, 422-510`
**Effort:** 0 min — no separate fix
**Fix:** Per the triage notes, this performance complaint is the duplicate of the resource-management (RM-2) and robustness (RB-9) angles on the same code. The RM-2 P4-issue below captures the "fresh socket connect every 250ms" cost; RB-9 (already P3) captures the "no exponential backoff" angle. Closing PF-V1.30.1-2 follows from either being addressed.

# P4 Hard — File as GitHub Issues

## P4-issue: EX-V1.30.1-1 — daemon_ping/status/reconcile near-identical 80-LOC copies
**Why an issue:** Refactor of three production daemon RPC functions; needs a small design pass for the helper signature (envelope shape, error tag, span name) before touching code. Low risk but worth a focused PR.

**Suggested labels:** `enhancement`, `tier-3`, `extensibility`

**Issue body draft:**
```
EX-V1.30.1-1: Extract `daemon_request<T>` to dedupe daemon_ping/status/reconcile

`daemon_ping`, `daemon_status`, and `daemon_reconcile` are three near-identical
~80-LOC functions in `src/daemon_translate.rs`. Each does the same work:

- socket connect → set_read_timeout → set_write_timeout
- write request line → flush
- read response line (64 KiB cap)
- parse envelope → check `status == "ok"` → extract `output`
- unwrap dispatch payload → deserialize sequence

The only differences are: (a) the `command` string in the request, (b) the
`tracing::info_span!` name, (c) the deserialized type, (d) the error tag in
`unwrap_dispatch_payload`. The 5-second timeout, 64 KiB read cap, and 6+
`tracing::warn!` `stage=` arms are duplicated verbatim.

Three is the threshold where duplication becomes its own bug surface — a
future change to the timeout default, or a new error path in
`unwrap_dispatch_payload`, has to be synced across all three. This is also
the daemon-client side of the EX-V1.29-1 (Commands trait, #1097) problem.

**Current shape:** `src/daemon_translate.rs:271-356` (ping), `:422-510` (status),
`:537-621` (reconcile). Three functions, each ~80 LOC, 90% byte-identical
across the body.

**Proposed direction:** Extract a single helper:

```rust
fn daemon_request<T: DeserializeOwned>(
    cqs_dir: &Path,
    command: &str,
    payload_label: &str,
    request_args: serde_json::Value,
) -> Result<T, String>
```

The three public entry points become 3-5 line shims that call the helper
with their command name and expected payload tag. Centralizes timeout,
read cap, span, and warn `stage=` arms so a future `daemon_gc` or
`daemon_invalidate` is a one-line addition. Composes with a
`daemon_request_with_args` overload for the `Reconcile { hook, args }` shape.

**Acceptance criteria:**
- `daemon_ping`, `daemon_status`, `daemon_reconcile` each ≤ 10 LOC.
- Existing tests in `src/daemon_translate.rs` (`*_mock_round_trip`) still
  pass without modification.
- Adding a hypothetical `daemon_invalidate` becomes one new wrapper plus
  the helper's existing infrastructure.
- `cargo build --features cuda-index` warning-clean (no dead-code on
  `unwrap_dispatch_payload` or any helper).

**Out of scope:**
- Changing the wire format or envelope shape.
- Changing `Result<T, String>` to a typed error (covered by API-V1.30.1-5,
  separate P2 fix).
- Adding a new daemon RPC.
```

## P4-issue: EX-V1.30.1-2 — BatchCmd dispatch hand-routed match (33 arms)
**Why an issue:** Refactor with a real architectural decision (extending the existing `for_each_batch_cmd_pipeability!` macro vs. a separate dispatch table); needs design discussion to keep the macro readable as the variant set grows.

**Suggested labels:** `enhancement`, `tier-3`, `extensibility`, `arch`

**Issue body draft:**
```
EX-V1.30.1-2: Drive BatchCmd dispatch from the macro table instead of a 33-arm match

Variant pipeability is now table-driven via `for_each_batch_cmd_pipeability!`
(issue #1137 fix, `src/cli/batch/commands.rs:372-447`): adding a `BatchCmd`
variant *requires* adding a `(Variant, bool)` row to the macro or the build
fails. But `dispatch()` 60 lines further down is still a hand-maintained
match — 33 arms — with no compile-time check that every variant has a
handler. Adding a new variant means:

(a) Add the variant — compile-enforced.
(b) Add the pipeability row — compile-enforced.
(c) Add the dispatch arm — NOT compile-enforced today (no `_` wildcard,
    so a missing arm fails). But a future refactor that adds `_ =>` would
    silently route new variants to nowhere.
(d) Write the handler.

Plus `Refresh` is special-cased outside `dispatch()` in `dispatch_via_view`
(line 1481), compounding the surgery cost: a new "side-effect" command
needs touches in `dispatch_via_view`, `dispatch`, and the pipeability
table.

**Current shape:** `src/cli/batch/commands.rs:503-636` (dispatch arms), with
six arms also calling `log_query` (covered separately by EX-V1.30.1-3,
already P3).

**Proposed direction:** Extend the existing macro table with a handler
function pointer per row:

```rust
for_each_batch_cmd!(
    (Search,   pipeable: false, handler: dispatch_search,   query_field: Some(query)),
    (Callers,  pipeable: true,  handler: dispatch_callers,  query_field: None),
    ...
);
```

The macro emits both `is_pipeable` and a single `dispatch_handler`
function. Side-effect commands like `Refresh` get a third column flag
(`needs_outer_lock`) so `dispatch_via_view` consults the same table
instead of an `if matches!()` special case.

**Acceptance criteria:**
- Adding a new `BatchCmd` variant requires editing exactly one row in
  the macro table plus writing the handler function.
- All 33 existing dispatch arms collapse into the macro table; `dispatch()`
  body shrinks to a generated match arm or a simple lookup.
- The `Refresh` / `Ping` / `Status` / `Reconcile` side-effect commands
  unify under the same macro-driven path; no `if matches!()` in
  `dispatch_via_view`.
- Tests for batch-mode dispatch (existing in `src/cli/batch/`) pass.
- `cargo expand --features cuda-index src::cli::batch::commands` produces
  the same dispatch shape as the current hand-rolled match (sanity check).

**Out of scope:**
- Changing the public `BatchCmd` enum shape.
- Migrating CLI dispatch (`cli/dispatch.rs`) — that's the EX-V1.29-1
  Commands trait work tracked in #1097.
- Adding new commands.
```

## P4-issue: EX-V1.30.1-4 — `write_slot_model` clobbers all non-`[embedding]` keys
**Why an issue:** Schema/extensibility issue; touches every future per-slot field. Needs design pass on whether to use `toml_edit` document round-trip or a structured `SlotConfig` builder. Also intersects with #1107 (slot create --model not persisted).

**Suggested labels:** `enhancement`, `tier-3`, `extensibility`, `schema`

**Issue body draft:**
```
EX-V1.30.1-4: Replace `write_slot_model` round-trip-clobber with section-preserving edit

`write_slot_model` (`src/slot/mod.rs:307-351`) emits
`format!("[embedding]\nmodel = {}\n", ...)` and overwrites the entire
slot.toml. The doc comment at `:300-302` acknowledges the issue and
hand-waves: *"Existing TOML keys outside [embedding] are not preserved —
slot.toml is owned by cqs."*

That hand-wave is fine for v1.29.1 (only one section) but expensive the
moment any future per-slot field lands:

- #1107 already filed: `slot create --model` doesn't persist the model.
  Fixing it via this code path requires read-modify-write, not the
  current write-only.
- Obvious next sections: `[reranker]`, `[splade]`, `[index].backend`,
  `[chunk].max_seq_len`, per-slot `[ignore]` overrides.
- Each addition would need: (a) field in `SlotConfigFile`, (b) extending
  `write_slot_model` to accept it, (c) renaming the function (it now
  writes more than the model), (d) auditing every existing slot.toml
  on upgrade.

**Current shape:** `src/slot/mod.rs:307-351` — single `format!()` body,
deserialize side at `:283-286` only reads `[embedding].model`.

**Proposed direction:** Two options, pick one:

(a) **`toml_edit::Document` round-trip** — read the existing file,
    parse with `toml_edit`, mutate the requested key, serialize back.
    Preserves comments and section ordering. Already in workspace
    (transitive of cargo metadata?); confirm and pull in if needed.

(b) **Structured `SlotConfig` builder** — extend `SlotConfigFile` to
    cover every section, deserialize-fully on read, serialize-fully
    on write. Loses comments but is type-safe.

Option (a) is the proper fix for "slot.toml owned by cqs but extensible";
option (b) is simpler if we accept comment loss.

**Acceptance criteria:**
- A slot.toml with hand-added keys (e.g. `[notes].project_id = "foo"`)
  survives `write_slot_model` unchanged.
- Adding a new `[reranker].preset` field requires zero changes to
  `write_slot_model` itself — just extend `SlotConfig` and call a
  `set_field("[reranker].preset", value)` helper.
- Existing tests in `src/slot/mod.rs` and `slot_create_default_smoke` /
  `slot_promote_smoke` pass.
- Fixing #1107 (slot create --model) becomes one call to the new
  function, not a redesign.

**Out of scope:**
- Migrating existing slot.toml files (they're forward-compatible; new
  keys are only read on demand).
- Adding actual new sections (reranker/splade) — separate features.
```

## P4-issue: EX-V1.30.1-5 — `check_request` hardcoded three-channel ladder
**Why an issue:** Auth surface refactor; needs design discussion on the channel-trait signature, ordering policy, and how `AuthOutcome` collapses. Pairs with the P1 SEC-7 leakage fix (CQ-V1.30.1-4 + AC-V1.30.1-5) — landing those first lets this refactor preserve the fix as a property of the query-channel impl.

**Suggested labels:** `enhancement`, `tier-3`, `extensibility`, `auth`

**Issue body draft:**
```
EX-V1.30.1-5: Replace `check_request` if/else ladder with `AuthChannel` trait + registry

`check_request` (`src/serve/auth.rs:269-321`) walks three explicit code
blocks in order: (1) `Authorization: Bearer …` header, (2) `cqs_token_<port>`
cookie, (3) `?token=…` query param. Each block has its own `for/loop`,
its own `ct_eq` call, its own success-return.

Adding a fourth channel — mTLS client cert (in the SECURITY threat model
already), API key for headless CI, session JWT for audit-trail integration,
SSO bearer — requires:

(a) New code block in `check_request`.
(b) Possibly a new `AuthOutcome` variant (see how `OkViaQueryParam`
    triggers a redirect).
(c) Sibling helper if the channel needs sanitization (the way
    `strip_token_param` strips `?token=`).
(d) Explicit ordering decision: does mTLS supersede a bearer header?

Today's three channels also have a known leakage gap (`strip_token_param`
not case-folding or percent-decoding — covered by P1 CQ-V1.30.1-4) that
*should* be a property of the channel module, not a free function bolted
onto the URI walk.

**Current shape:** `src/serve/auth.rs:269-321` (check_request) +
`:246-260` (strip_token_param) + `:323-332` (AuthOutcome enum). Three
hand-rolled blocks, one sibling helper, one variant for "channel that
needs post-auth redirect."

**Proposed direction:**

```rust
trait AuthChannel: Send + Sync {
    fn check(&self, req: &Request, expected: &AuthToken) -> Option<ChannelMatch>;
    fn sanitize_request(&self, req: &mut Request);  // default: no-op
    fn name(&self) -> &'static str;
}

struct AuthChannelRegistry {
    channels: Vec<Box<dyn AuthChannel>>,  // priority order
}
```

Each existing channel becomes one impl (~30 LOC):
- `BearerHeaderChannel`
- `CookieChannel` (port-aware)
- `QueryParamChannel` (sanitize_request strips `?token=`)

`AuthOutcome` collapses into `Option<ChannelMatch { needs_redirect: bool }>`
— single decision point. Adding mTLS/API key/JWT becomes "implement
the trait, add to the registry constructor."

**Acceptance criteria:**
- Each of the three existing channels lives in its own ~30-LOC impl
  block; `check_request` becomes a 5-line registry walk.
- The P1 SEC-7 fix (`strip_token_param` case-fold + percent-decode)
  lives entirely inside `QueryParamChannel::sanitize_request`.
- Ordering policy is explicit and documented (header > cookie > query,
  matching today's behavior).
- Auth tests in `tests/cli_serve_auth_test.rs` pass without modification.
- Adding a stub `MtlsChannel` in a follow-up PR is one new file plus
  one line in the registry constructor.

**Out of scope:**
- Actually implementing mTLS / API key / JWT.
- Changing the wire shape of the cookie or query param.
- Audit logging of which channel matched (separate observability work).
```

## P4-issue: EX-V1.30.1-6 — Reconcile fingerprint is `(path, mtime)` only
**Why an issue:** Schema migration plus reconcile-logic rewrite; spans Store, watch reconcile, GC, and any tool that consumes `indexed_file_origins`. Hard work that needs a design doc before code, especially around the fingerprint policy enum.

**Suggested labels:** `enhancement`, `tier-3`, `data-integrity`, `schema`

**Issue body draft:**
```
EX-V1.30.1-6: Add content-hash + size to reconcile fingerprint (schema v23)

`run_daemon_reconcile` (`src/cli/watch/reconcile.rs:84-134`) decides
"is this file divergent?" by `disk > stored` mtime comparison only
(line 124). Two well-known reconciliation bugs slip through:

(a) **Coarse-mtime collisions.** WSL DrvFS / NTFS / HFS+ / SMB mount
    points have ≥1 s mtime resolution. Two saves within the same
    second produce identical mtimes; reconcile skips the second one.
    `events.rs:85-100` has a per-FS workaround for the inotify path
    (the `is_wsl_drvfs_path` toggle), but reconcile's whole-tree-walk
    path doesn't compensate.

(b) **Content-identical-but-mtime-bumped.** Formatter passes, `touch`,
    branch checkouts that restore the same content all re-trigger
    full embedder cost on every `git checkout`. ~3-5k unnecessary
    reembeds per branch flip on a mid-size repo.

The fingerprint shape is hardcoded into both the SQLite column choice
(`indexed_files.last_indexed_mtime` is a single i64) and the in-memory
map type (`HashMap<String, Option<i64>>` from `indexed_file_origins`).

**Current shape:**
- `src/store/chunks/staleness.rs:627-637` — `indexed_file_origins`
  returns `HashMap<String, Option<i64>>`.
- `src/cli/watch/reconcile.rs:84-134` — divergence is a single
  `disk > stored` predicate.
- Schema: `chunks.source_mtime` column only.

**Proposed direction:**

```rust
struct FileFingerprint {
    mtime: Option<i64>,
    size: Option<u64>,
    content_hash: Option<[u8; 32]>,
}

enum FingerprintPolicy {
    MtimeOnly,        // current behavior, fast path
    MtimeOrHash,      // recommended default
    HashOnly,         // for `cqs index --strict`
}

impl FileFingerprint {
    fn matches(&self, other: &Self, policy: FingerprintPolicy) -> bool { ... }
}
```

- Schema v23 + migration: add nullable `chunks.source_size INTEGER`
  and `chunks.source_content_hash BLOB` columns.
- `indexed_file_origins` returns `HashMap<String, FileFingerprint>`.
- Reconcile passes the policy to one helper instead of inlining the
  comparison at 5 callsites.
- `cqs index --strict` opt-in for hash-on-walk; default path stays
  mtime-cheap.

**Acceptance criteria:**
- Schema v23 migration lands with backfill (NULL hash + size for
  pre-migration rows; first re-embed populates them).
- `cqs index --force` populates new columns on every chunk.
- Reconcile reduced to one helper call: `disk_fp.matches(stored_fp,
  policy)`.
- A test covering coarse-mtime FS (synthetic stat with 1 s rounding)
  uses the policy to detect divergence on identical-mtime files.
- Existing reconcile tests pass.
- Eval R@5 on test/dev fixtures unchanged (this is plumbing, not a
  scoring change).

**Out of scope:**
- Adding the strict-hash flag to `cqs watch` (CLI work, follow-up).
- Cross-slot summary reuse (separate workflow already documented in
  feedback_summary_cross_slot.md).
- Removing the in-memory `HashMap` materialization (covered by RM-5
  P4-issue below).
```

## P4-issue: EX-V1.30.1-8 — Reranker is a concrete struct, no trait
**Why an issue:** Deep refactor; touches every callsite that holds `&Reranker` (search, search_filtered, daemon batch, eval, doctor). Needs design discussion on the trait surface (especially the `expects_token_type_ids` private state) before code.

**Suggested labels:** `enhancement`, `tier-3`, `extensibility`, `eval`

**Issue body draft:**
```
EX-V1.30.1-8: Extract `Reranker` trait + ONNX impl + LlmReranker / NoopReranker

`Reranker` (`src/reranker.rs:108-167`) is a concrete struct that bakes
"ONNX session + tokenizer + token_type_ids feature-detection" directly
into the type. `RerankerError::Inference(String)` is itself ONNX-shaped.

Adding any non-ONNX scoring family — LLM-judge reranker via the
existing `BatchProvider` trait, BM25-on-content baseline for IR-eval
parity, dot-product reranker over a different embedding model, or a
no-op pass-through for benchmarking — requires touching every callsite
that holds a `&Reranker` (search, search_filtered, daemon batch, eval
harness, doctor) and either adding an enum or duplicating each callsite.

The codebase already extracted `BatchProvider` for LLMs
(`src/llm/provider.rs:42`) — same shape works for rerankers. The
known BERT-vs-RoBERTa input-shape divergence
(`expects_token_type_ids: Mutex<Option<bool>>` at lines 121-124) is
itself a within-implementation polymorphism leak; cleaner trait split
would put that state inside the impl, not on the trait surface.

**Current shape:** `src/reranker.rs:108-167` (struct + ctor),
`:172-211` (rerank), `:212-...` (rerank_with_passages). Single
concrete type, ONNX-only.

**Proposed direction:**

```rust
pub trait Reranker: Send + Sync {
    fn rerank(
        &self,
        query: &str,
        results: &mut Vec<SearchResult>,
        limit: usize,
    ) -> Result<(), RerankerError>;

    fn rerank_with_passages(
        &self,
        query: &str,
        passages: &mut Vec<RerankPassage>,
        limit: usize,
    ) -> Result<(), RerankerError>;
}

pub struct OnnxReranker { /* current struct fields */ }
impl Reranker for OnnxReranker { ... }

pub struct NoopReranker;
impl Reranker for NoopReranker { fn rerank(...) { Ok(()) } ... }

pub struct LlmReranker { provider: Arc<dyn BatchProvider> }
impl Reranker for LlmReranker { ... }
```

Hold rerankers as `Arc<dyn Reranker>` everywhere — the existing
Mutex-around-session pattern means `dyn` overhead is below the noise
floor. The `expects_token_type_ids` feature detection becomes private
to `OnnxReranker`.

**Acceptance criteria:**
- `Reranker` trait + `OnnxReranker` impl with current behavior.
- `NoopReranker` shipped (eval-harness ablation use case).
- `LlmReranker` skeleton that delegates to `BatchProvider` (no
  production use; just proves the trait surface).
- All call sites switch from `&Reranker` to `Arc<dyn Reranker>`.
- Eval R@5 on test/dev fixtures unchanged with `OnnxReranker`
  (this is purely a refactor; no scoring change).
- An eval-harness ablation switch (`--reranker none|onnx`) lands
  with `NoopReranker` for instant comparison.

**Out of scope:**
- Actual LLM reranker production deployment (just the skeleton lands).
- BM25 reranker (separate feature, future PR).
- Changing `RerankerError` shape from `Inference(String)` —
  follow-up if it becomes a constraint.
```

## P4-issue: SEC-V1.30.1-5 — `trust_level: "user-code"` for vendored content (proper fix)
**Why an issue:** The doc-only mitigation (in P4-trivial above) is a stop-gap. The proper fix is a path-prefix denylist, per-chunk `vendored: bool`, and downgraded trust level. That requires schema + indexer + JSON-shape changes and careful agent-facing impact analysis.

**Suggested labels:** `enhancement`, `tier-3`, `security`, `schema`

**Issue body draft:**
```
SEC-V1.30.1-5 (proper fix): Tag vendored chunks at index time, downgrade trust_level

When `--include-refs` is unset, search results emit
`trust_level: "user-code"` unconditionally
(`src/store/helpers/types.rs:172-196`). The "user-code" claim is
structural — it tracks "did this come from a `cqs ref` reference index"
not "is this code authored by the project owner."

Vendored/copied third-party code committed into the project tree, and
content surfaced by `cqs notes` from `docs/notes.toml`, all emit as
`user-code` despite being exactly the indirect-prompt-injection surface
the trust-level field exists to flag. SECURITY.md explicitly calls out
vendored upstream content as a payload vector but the trust-level
signal cannot distinguish vendored from authored code.

The doc-only mitigation (clarifying SECURITY.md that "user-code" means
"from project store" not "authored by user") is landing as a P4-trivial
fix. This issue tracks the proper structural fix.

**Current shape:** `src/store/helpers/types.rs:172-196`
(`to_json_with_origin`) emits `"trust_level": "user-code"` whenever
`ref_name.is_none()`. Per-chunk source classification stops at "is
this from a reference?".

**Proposed direction:**

(1) Add `vendored: bool` column on `chunks` (schema v23 if
    EX-V1.30.1-6 lands first, else v23 carrying just this field).
(2) Compile a default path-prefix denylist at index time:
    `["vendor/", "third_party/", "node_modules/", ".cargo/", "target/",
    "dist/", "build/"]`. Make it `.cqs.toml`-overridable
    (`[index].vendored_paths`).
(3) During `enumerate_files` / index pipeline, mark any chunk whose
    `origin` starts with a denylist prefix as `vendored: true`.
(4) `to_json_with_origin` downgrades to `"third-party-code"` for
    vendored chunks (or a third tier `"vendored-code"` if we want
    to keep "third-party-code" reserved for `cqs ref` refs).

**Acceptance criteria:**
- Schema v23 (or v24, depending on EX-V1.30.1-6 ordering) adds
  `chunks.vendored` column.
- Default vendor-prefix list documented in CONTRIBUTING.md and
  matched by an index-time test.
- `.cqs.toml` `[index].vendored_paths` override honoured by
  index pipeline.
- A new chunk with `origin = "vendor/oss-lib/foo.rs"` emits
  `trust_level: "vendored-code"` in search/scout/onboard JSON.
- SECURITY.md trust-level table updated to document the new
  category.
- Eval R@5 unchanged (vendored chunks still show up in results;
  only the `trust_level` JSON field differs).

**Out of scope:**
- Excluding vendored code from indexing entirely (separate config;
  current behavior preserves it for searchability).
- Per-chunk attribution beyond the binary user/vendored split.
- The doc-only stop-gap fix (lands in P4-trivial).
```

## P4-issue: SEC-V1.30.1-6 — `cqs ref add` accepts symlinked source path with no audit
**Why an issue:** Security-relevant input validation that needs a clear policy decision (warn vs. refuse) and tests covering the cross-tree symlink case. Not a one-liner.

**Suggested labels:** `bug`, `tier-3`, `security`

**Issue body draft:**
```
SEC-V1.30.1-6: Surface symlink resolution in `cqs ref add` source path

`cmd_ref_add` (`src/cli/commands/infra/reference.rs:130-150`)
canonicalizes the `--source` path via `dunce::canonicalize` but does
not compare the user-supplied path against the resolved root. If the
user runs `cqs ref add foo /home/me/projects/foo` against a path
that's a symlink to `/some/other/dir/proprietary-code/`, the indexed
corpus is the *target* of the symlink — not what the user asked to
index.

Nothing in the ref add flow flags the redirection. The reference is
persisted into `.cqs.toml` as `source = <canonical-resolved-path>`,
so a follow-up `cqs ref list` shows the resolved path — but if the
user's mental model is "I added /home/me/projects/foo," the reindex
behavior is surprising.

Concrete failure case: operators who symlink
`vendored-monorepo-pull/ → ~/work/customer-A-private/` to "test cqs
ref" and silently end up with a customer-content reference index.
Also relevant if `cqs ref add` is used inside CI/automation that
takes `--source` from a config file controlled by a less-trusted
contributor.

**Current shape:** `src/cli/commands/infra/reference.rs:130-150` —
`source = dunce::canonicalize(source)?;` with no comparison or
warning if it differs from the user input.

**Proposed direction:**

(1) Compare `source_input` (raw user arg, after `Path::new`) to
    `dunce::canonicalize(source_input)`. If they differ, log a
    `tracing::warn!` and emit a `warnings: ["source path resolved
    via symlink to ..."]` field on the JSON return.
(2) Optionally: add `--allow-symlink-source` opt-in flag; refuse
    by default with a clear error pointing at the flag and the
    resolved target.

Option (1) is the smaller change with adequate operator visibility;
option (2) is the strict-fail variant for CI use.

**Acceptance criteria:**
- A symlinked source path triggers a `tracing::warn!` with both
  user-supplied and resolved paths.
- JSON output includes the warning field.
- A new test in `tests/cli_ref_test.rs` (or unit in
  `reference.rs::tests`) creates a symlink-source tempdir,
  invokes `cmd_ref_add`, asserts the warning fires and the
  index DB lives at the resolved path.
- README / SECURITY.md updated to document the resolution behavior.

**Out of scope:**
- Canonicalizing symlinks *inside* the source tree during the walk
  (`enumerate_files` already passes `follow_links(false)`).
- Cross-platform symlink semantics on Windows (this is a Linux/macOS
  feature for now; Windows symlinks need admin and are rare).
```

## P4-issue: SEC-V1.30.1-7 — `LocalProvider` redirect policy doesn't enforce same-origin
**Why an issue:** Security-grade behavior with a behavioural-correctness gap (the comment claims same-origin but the code doesn't enforce it). Needs custom policy closure + tests + verifying reqwest's strip-on-redirect behavior across the pinned version.

**Suggested labels:** `bug`, `tier-3`, `security`, `auth`

**Issue body draft:**
```
SEC-V1.30.1-7: Enforce same-origin redirects on bearer-bearing LLM requests

`LocalProvider` HTTP client (`src/llm/local.rs:124-129`) uses
`Policy::limited(2)` for redirects. The change rationale comment
claims "Same-origin HTTP→HTTPS redirects on bind-localhost are benign"
but `Policy::limited(2)` does *not* enforce same-origin. A misconfigured
`CQS_LLM_API_BASE` (load balancer that 302s from `http://internal-llm/`
→ `http://attacker-controlled-origin/v1/chat/completions`) follows
the redirect.

reqwest 0.12.x strips `Authorization` cross-origin by default — so
this is currently observability-grade (silent 401 instead of "redirect
to other origin, bearer stripped"). But:

(a) The strip is silent; operators see infinite-401 loops instead
    of a clean fail-fast.
(b) A future reqwest bump (or a misconfigured global default) that
    re-enables cross-origin auth header propagation turns this into
    a credential-leak path.
(c) The comment in the code claims a property the code doesn't
    enforce — that's a maintainability bug at minimum.

**Current shape:** `src/llm/local.rs:124-129` —
`.redirect(reqwest::redirect::Policy::limited(2))`. Doctor probe
(`src/llm/local.rs:435-437`) sends `Authorization: Bearer <key>`
header without same-origin guard.

**Proposed direction:**

```rust
let same_origin_policy = reqwest::redirect::Policy::custom(|attempt| {
    let prev = attempt.previous().last();
    let next = attempt.url();
    if let Some(prev_url) = prev {
        if prev_url.origin() != next.origin() {
            tracing::warn!(
                from = %prev_url,
                to = %next,
                "Refusing cross-origin redirect on bearer-bearing request"
            );
            return attempt.stop();
        }
    }
    if attempt.previous().len() >= 2 {
        return attempt.stop();
    }
    attempt.follow()
});

Client::builder()
    .redirect(same_origin_policy)
    .timeout(timeout)
    ...
```

Apply the same policy to the doctor probe.

**Acceptance criteria:**
- Cross-origin redirects on `LocalProvider` produce a clear
  `tracing::warn!` and a fail-fast error, not a silent 401 loop.
- A test using `wiremock` or `httpmock` simulates a cross-origin
  302; assert the request errors with a redirect-policy diagnostic.
- Doctor probe matches the same policy.
- The misleading comment at `:124-129` is replaced with text that
  matches what the code enforces.

**Out of scope:**
- mTLS or SAN-based origin matching (origin string compare only).
- Changing the limit on same-origin redirect chain length
  (stays at 2).
```

## P4-issue: PB-V1.30.1-4 — `open_browser` on WSL launches Linux browser via `xdg-open`
**Why an issue:** WSL platform behavior with multiple fallbacks (`wslview` / `cmd.exe` / `xdg-open`) and a clear ordering. Not a one-liner because it needs `is_wsl()` integration into `open_browser` plus testing across WSL2 with and without `wslu` installed.

**Suggested labels:** `bug`, `tier-3`, `platform-wsl`

**Issue body draft:**
```
PB-V1.30.1-4: Detect WSL in `open_browser` and prefer Windows-side default

WSL satisfies `cfg(target_os = "linux")`, so `open_browser`
(`src/cli/commands/serve.rs:99-132`) hits the Linux branch and
spawns `xdg-open <url>`. On a fresh WSL install there is no Linux
GUI / browser, so `xdg-open` either errors with `xdg-open: no method
available` or hangs trying to launch a non-existent browser.

The user sees `WARN: --open requested but failed to launch browser`
and has to copy-paste the URL into Windows. The intended behavior
on WSL is `cmd.exe /c start <url>` (or `wslview <url>` if `wslu` is
installed) which hands the URL to the Windows default browser.

`is_wsl()` already exists in `src/config.rs:47` and is used elsewhere
— `open_browser` ignores it.

**Current shape:** `src/cli/commands/serve.rs:117-130` — single
`#[cfg(target_os = "linux")]` arm, hardcoded `xdg-open`.

**Proposed direction:**

```rust
#[cfg(target_os = "linux")]
{
    if cqs::config::is_wsl() {
        // 1. Try `wslview` (handles auth-token URLs cleanly).
        // 2. Fall back to `cmd.exe /c start "" "<url>"` (interop layer
        //    translates the call from WSL Linux).
        // 3. Final fallback: xdg-open on the off chance a Linux GUI exists.
        for &cmd in &["wslview", "cmd.exe", "xdg-open"] {
            let mut args = match cmd {
                "cmd.exe" => vec!["/C", "start", "", url],
                _         => vec![url],
            };
            ... try-spawn ...
        }
    } else {
        std::process::Command::new("xdg-open")...
    }
}
```

A successful spawn on any of the three is enough; bail on first
success. Continue silently to the next on `Command::status()` failure.

**Acceptance criteria:**
- On WSL2 without `wslu` installed, `cqs serve --open` hands the
  URL to the Windows default browser via `cmd.exe /c start ...`.
- On WSL2 with `wslu` installed, `wslview` is preferred (cleaner
  arg handling).
- On native Linux, behavior is unchanged.
- A unit test in `serve.rs::tests` (or a manual smoke checklist
  documented in CONTRIBUTING.md) covers the three-path fallback.

**Out of scope:**
- Native macOS handling (`open` is already correct).
- Windows handling (`cmd /C start "" "<url>"` already correct).
- Detecting *which* Windows browser will launch.
```

## P4-issue: PB-V1.30.1-5 — `events.rs` mtime-equality wrong on macOS HFS+ and SMB/NFS
**Why an issue:** Cross-platform watch correctness. Touches the same predicate also addressed by EX-V1.30.1-6 (reconcile fingerprint) but on the inotify-event path. Should pair with that issue or land first since it's the smaller change.

**Suggested labels:** `bug`, `tier-3`, `platform`, `data-integrity`

**Issue body draft:**
```
PB-V1.30.1-5: Treat any coarse-mtime FS as "always reindex on tie", not just WSL drvfs

`events.rs` mtime-equality skip (`src/cli/watch/events.rs:85-102`)
toggles between strict `<` (WSL drvfs) and `<=` (everything else):

```rust
let coarse_fs = cqs::config::is_wsl_drvfs_path(&path);
let stale = state.last_indexed_mtime.get(rel).is_some_and(|last| {
    if coarse_fs { mtime < *last } else { mtime <= *last }
});
```

Comment claims "On Linux/macOS we keep the original `<=` because
sub-second mtimes there are reliable and equality genuinely means
same content." This is true for ext4 / APFS / btrfs.

It is **false** for:
- HFS+ on macOS (1-second mtime resolution) — still common on
  external drives, Time Machine restores, macOS < 10.14.
- SMB/NFS shares mounted on Linux or macOS — typically 1-2 second
  mtime resolution depending on server.
- FAT32 USB / SD-card mounts on Linux.

Two saves of the same file within the same second produce identical
mtimes; the watch loop sees the second save's mtime equal to
`last_indexed_mtime` and skips the reindex. The user's last edit
silently doesn't make it into the index.

**Current shape:** `src/cli/watch/events.rs:85-102` — `is_wsl_drvfs_path`
is the only gate triggering strict `<`.

**Proposed direction:**

Replace the predicate from "is WSL drvfs" to "is the cached mtime
within `coarse_fs_resolution()` of `mtime`":

```rust
fn coarse_fs_resolution(path: &Path) -> Duration {
    if cqs::config::is_wsl_drvfs_path(path) { Duration::from_secs(2) }
    else if is_macos_hfs(path) { Duration::from_secs(1) }
    else if is_remote_mount(path) { Duration::from_secs(1) }
    else { Duration::from_millis(0) }
}

let stale = state.last_indexed_mtime.get(rel).is_some_and(|last| {
    let resolution = coarse_fs_resolution(&path);
    let delta = mtime.duration_since(*last).unwrap_or_default();
    delta > resolution
});
```

Treat any cached value within `coarse_fs_resolution` of `mtime` as
ambiguous and force-reindex. Cheap conservative default; cost is at
most one redundant reindex on rapid re-saves on fine-grained FS, vs.
silent missed reindexes on coarse ones.

**Acceptance criteria:**
- HFS+, SMB, NFS path detection lands in a new helper.
- Two saves within `coarse_fs_resolution` on a coarse-mtime FS
  produce two reindexes, not one.
- On ext4 / APFS / btrfs, behavior is unchanged (resolution = 0).
- A unit test in `events.rs::tests` covers the predicate's
  decision matrix using synthetic mtime/now/cached values.

**Out of scope:**
- Detecting per-FS resolution by `statvfs` flags or `f_frsize`
  inspection (use mount-type heuristics).
- Reconcile path mtime equality (covered by EX-V1.30.1-6).
- Adding a `--strict-mtime` CLI flag (env var only if needed).
```

## P4-issue: PF-V1.30.1-3 — Periodic GC and reconcile do back-to-back tree walks
**Why an issue:** Performance refactor with shared cache between two callers; touches both gc.rs and reconcile.rs scheduling and needs verification that the shared HashSet doesn't change reconcile's mtime-comparison semantics.

**Suggested labels:** `enhancement`, `tier-3`, `performance`

**Issue body draft:**
```
PF-V1.30.1-3: Share `enumerate_files` walk between periodic GC and reconcile

When the daemon idles ≥ `daemon_periodic_gc_idle_secs()` (default 60s),
both periodic GC and periodic reconcile may fire in the same tick:

- GC's `Duration::from_secs(daemon_periodic_gc_interval_secs())`
  (default 1800s) gates its walk.
- Reconcile's `daemon_reconcile_interval_secs()` (default 30s) gates
  the second walk.

Their idle gates are identical, so on tick boundaries that satisfy
both intervals, the daemon walks the entire working tree **twice in
succession**, each walk doing per-file canonicalization through
`dunce::canonicalize`.

On a 17k-chunk corpus with ~3-5k unique source files on WSL `/mnt/c/`,
each walk is ~1s wall (per the docstring on
`DAEMON_RECONCILE_INTERVAL_SECS_DEFAULT`); doing it twice back-to-back
is a 2s contention window every 30 minutes. Even when only one fires,
both call paths build a `HashSet<PathBuf>` of disk files (gc.rs:211,
reconcile.rs:84+95), so the same data is materialized twice in the
same tick whenever both run.

**Current shape:**
- `src/cli/watch/mod.rs:1198-1283` — idle-tick gating + dispatch.
- `src/cli/watch/gc.rs:209` — first `enumerate_files` callsite.
- `src/cli/watch/reconcile.rs:74` — second `enumerate_files` callsite.

**Proposed direction:**

Lift the walk out of both call sites:

```rust
// In the idle-tick block, before either GC or reconcile fires:
let disk_files: Option<Arc<HashSet<PathBuf>>> = if gc_due || reconcile_due {
    Some(Arc::new(enumerate_files(...)?.collect()))
} else {
    None
};

if gc_due {
    run_periodic_gc(&store, disk_files.as_deref()...);
}
if reconcile_due {
    run_daemon_reconcile_with_walk(&store, disk_files.as_deref()...);
}
```

Both already operate on the same set of disk files; the only daylight
is reconcile checks each file's mtime and GC checks its existence —
both derivable from one walk.

**Acceptance criteria:**
- When both intervals fire on the same tick, only one
  `enumerate_files` walk happens.
- When only one fires, behavior is unchanged.
- GC's `prune_missing` still operates correctly (consumes the
  shared set).
- Reconcile's mtime-comparison loop still operates correctly
  (consumes the shared set + still calls `metadata()` per-file
  for mtime).
- A test in `gc.rs::tests` or `reconcile.rs::tests` asserts that
  back-to-back invocations with `disk_files: Some(shared_set)` do
  not call `enumerate_files` again.

**Out of scope:**
- Streaming the walk (covered by RM-5 P4-issue).
- Changing the interval defaults.
- Eliminating per-file `dunce::canonicalize` cost (separate fix).
```

## P4-issue: PF-V1.30.1-8 — `indexed_file_origins` SELECT DISTINCT silent overwrite
**Why an issue:** Subtle data-shape bug (silent overwrite when DISTINCT pairs differ on mtime) that needs a query rewrite plus a test pinning the deterministic-MAX behavior. Pairs with EX-V1.30.1-6 (fingerprint refactor) but is independently mergeable.

**Suggested labels:** `bug`, `tier-3`, `data-integrity`, `performance`

**Issue body draft:**
```
PF-V1.30.1-8: Replace `SELECT DISTINCT origin, source_mtime` with `GROUP BY origin, MAX(mtime)`

`indexed_file_origins` (`src/store/chunks/staleness.rs:627-637`)
runs:

```sql
SELECT DISTINCT origin, source_mtime FROM chunks WHERE source_type='file'
```

and collects into `HashMap<String, Option<i64>>`. If a file's chunks
were written across two upserts at different mtimes (a known edge
case during partial reindex failures, or transient writes during a
watch tick), `rows.into_iter().collect::<HashMap<_,_>>()` arbitrarily
picks the **last** one in iteration order — silently dropping the
earlier mtimes.

From the reconcile caller's perspective, the wrong stored mtime
causes either a missed reindex (if the chosen mtime happens to be
≥ disk mtime) or a spurious one. Order is undefined per SQL spec —
SQLite happens to be fairly deterministic but the contract isn't
guaranteed.

The DISTINCT also does extra work: it returns up to chunks-per-file
rows when the caller wants one row per file. For a 17k-chunk corpus
with 3k files at avg 5 chunks/file, that's 17k rows scanned and
collapsed into a 3k-entry HashMap.

**Current shape:** `src/store/chunks/staleness.rs:627-637`.

**Proposed direction:**

```sql
SELECT origin, MAX(source_mtime) FROM chunks
WHERE source_type='file'
GROUP BY origin
```

- One row per origin.
- Deterministic: returns the most-recent stored mtime, which is
  the semantically correct value for reconcile's `disk > stored`
  predicate.
- Fewer rows materialized.

**Acceptance criteria:**
- Query returns exactly one row per (origin, source_type='file').
- Returned mtime is the MAX across all chunks for that origin.
- A test in `staleness.rs::tests` writes two chunks for the same
  origin at different `source_mtime` values, asserts
  `indexed_file_origins` returns the larger.
- Reconcile tests pass without modification.
- Eval R@5 unchanged (this is a metadata query, not a scoring path).

**Out of scope:**
- Changing the `HashMap<String, Option<i64>>` return type to
  `FileFingerprint` (covered by EX-V1.30.1-6).
- Adding a content-hash column (covered by EX-V1.30.1-6).
```

## P4-issue: RM-2 — `wait_for_fresh` opens fresh socket every 250ms for up to 600s
**Why an issue:** Resource-management refactor with two competing approaches (server-side wait via `tokio::sync::Notify` vs. client-side connection reuse). Needs design discussion before code; partly overlaps with PF-V1.30.1-2 and RB-9.

**Suggested labels:** `enhancement`, `tier-3`, `performance`, `resource-management`

**Issue body draft:**
```
RM-2: Replace `wait_for_fresh` 250ms-poll with persistent connection or server push

`wait_for_fresh` (`src/daemon_translate.rs:660-679`) is the shared
client-side polling primitive for `cqs status --watch-fresh --wait`
and `cqs eval --require-fresh`. Each iteration calls `daemon_status`,
which opens a fresh `UnixStream::connect`, sets read/write timeouts,
writes a 30-byte JSON request, reads a 64 KiB-bounded response, and
drops the stream.

With the default `--require-fresh-secs=600`, a stuck-stale tree
triggers up to **2,400 socket connects + 2,400 JSON round trips** in
a single eval gate. None of this is catastrophic — Unix sockets are
cheap — but for a primitive that's now on the hot path of #1182 (the
freshness gate), the cost shape is wrong.

On the daemon side, each connection: (1) wakes the accept loop's
WouldBlock-poll thread, (2) spawns a *fresh OS thread* (per RM-1 —
no thread pool), (3) takes the `BatchContext` mutex to dispatch
`status`, (4) RwLock-reads the snapshot, (5) tears the thread down.
Two concurrent `eval --require-fresh` runs (real scenario when an
agent batch fans out) easily generate 4-5k connect-spawn-teardown
cycles in a 60s wait.

This issue subsumes PF-V1.30.1-2 (perf angle on the same code) and
pairs with RB-9 (no-exponential-backoff angle, P3).

**Current shape:** `src/daemon_translate.rs:660-679` (poll loop) +
`:438` (per-call `UnixStream::connect`).

**Proposed direction:** Two complementary fixes — pick (a) for
simplicity, or both for full eradication.

(a) **Persistent connection:** Keep the `UnixStream` open across
    polls inside `wait_for_fresh` and reuse it for subsequent
    `status` requests. One connect + N writes + N read_lines
    until either fresh or timeout. ~30-line change confined to
    `daemon_translate.rs`.

(b) **Server-side wait:** Add a `status --wait <secs>` daemon
    command that blocks server-side on a `tokio::sync::Notify`
    flipped by the watch loop when `state` transitions to `fresh`,
    then returns. One round-trip total instead of N. Bigger
    daemon-side change but eliminates polling entirely.

Both pair well with RB-9's exponential-backoff fix (300ms → 600 →
1.2s → 2.4s with cap) for the case where polling is preferred.

**Acceptance criteria:**
- A 60-second stale-then-fresh wait produces ≤ 1 socket
  connection (option a) or ≤ 2 (option b) — verified via a
  test that counts mock `accept()` calls.
- Existing tests
  (`wait_for_fresh_returns_fresh_on_first_poll` etc.) pass.
- A new test pins the connection-count contract.
- `cqs eval --require-fresh` is no slower on the realistic
  Stale → Fresh path.

**Out of scope:**
- Inotify-based push (separate design; option c in audit notes).
- Daemon thread pool work (RM-1 is a separate finding).
- Removing the `wait_for_fresh` helper entirely.
```

## P4-issue: RM-5 — Reconcile holds entire repo's filename set in RAM every 30s
**Why an issue:** Resource-management refactor that touches `enumerate_files` shape (Vec → impl Iterator) and reconcile's loop shape; pairs with PF-V1.30.1-3 (shared walk) but is independently scoped. Needs design pass to avoid breaking GC's existing consumer.

**Suggested labels:** `enhancement`, `tier-3`, `resource-management`, `scaling`

**Issue body draft:**
```
RM-5: Stream `enumerate_files` walk; query indexed origins per-file in reconcile

`run_daemon_reconcile` (`src/cli/watch/reconcile.rs:74-90`) runs every
30s by default when the daemon is idle. Each tick:

1. `enumerate_files(root, &exts, no_ignore)` returns a `Vec<PathBuf>`
   materialized via `walker.collect::<Vec<_>>()` — the **entire**
   tree's matching files in one allocation (lib.rs:618). For a
   100k-file monorepo at avg 80 B/PathBuf, that's ~8 MB.

2. `indexed_file_origins()` returns a `HashMap<String, Option<i64>>`
   with one entry per indexed source file — typically ~30k entries
   × ~120 B = ~3.5 MB.

3. Both are held simultaneously through the loop body that does
   set-difference and metadata() probes.

Every 30s, ~12 MB allocated, walked, and dropped. Not a leak — the
allocations are scoped — but it's repetitive heap churn the watch
daemon pays *forever in idle*, and it doesn't scale: a 1M-file
Chromium-class repo would hit ~120 MB transient per tick.

#1182's whole-tree-walk-on-idle premise should not break on a
1M-file repo.

**Current shape:**
- `src/cli/watch/reconcile.rs:74-90` — both materializations.
- `src/lib.rs:618` — `enumerate_files` returns `Vec<PathBuf>`.

**Proposed direction:**

Stream the disk walk: change `enumerate_files` to expose
`enumerate_files_iter(root, exts, no_ignore) -> impl
Iterator<Item = anyhow::Result<PathBuf>>`. The Vec-collecting
version stays as a thin wrapper for callers that genuinely need
the count.

In reconcile, switch `for rel in disk_files` to consume the iterator
directly, prepared-statement-querying `chunks.source_mtime WHERE
origin = ?` per file (or chunked in 1k batches) instead of
pre-loading the full `indexed` HashMap. Memory drops to
O(batch_size) regardless of repo size, and the walk + DB lookups
overlap in time.

**Acceptance criteria:**
- `enumerate_files_iter` exists and the existing
  `enumerate_files` is its `.collect::<Vec<_>>()` wrapper.
- Reconcile peak memory drops to O(batch_size) — verified via
  a synthetic 100k-file tempdir test that asserts steady-state
  RSS doesn't grow with file count.
- Per-file SQL lookup batched in groups of 1k via a prepared
  statement; not 100k individual `fetch_one` round trips.
- Reconcile correctness tests
  (`reconcile_detects_bulk_modify_burst` etc.) pass.
- GC and other `enumerate_files` consumers are unchanged.

**Out of scope:**
- Sharing the walk between GC and reconcile (covered by
  PF-V1.30.1-3).
- Adding a memory budget knob (env var). Streaming is the
  budget.
- Eliminating per-file `dunce::canonicalize` in the walk
  (separate concern).
```

## P4-issue: TC-HAP-1.30.1-6 — `process_file_changes` zero direct tests
**Why an issue:** Test-infra work; the function is the central watch-loop reindex path and "no direct tests" maps to "needs a test harness with a stub embedder" — that scaffolding is the actual work. Hard because the function takes an embedder, store, and config and spans the entire pending-files drain path.

**Suggested labels:** `tests`, `tier-3`, `test-coverage`

**Issue body draft:**
```
TC-HAP-1.30.1-6: Add direct tests for `process_file_changes` watch-loop drain

`process_file_changes` (`src/cli/watch/events.rs:131-300+`) is the
function the watch daemon calls every cycle to drain `pending_files`
into the index. It has zero direct tests.

`grep -rn process_file_changes src` returns three production call
sites in `watch/mod.rs` (lines 1125, 1254, 1276) and zero test sites.
The `reconcile.rs::tests::reconcile_detects_bulk_modify_burst` test
exercises *queueing* into `pending_files`, then asserts the snapshot
reads Stale — but never invokes `process_file_changes` to drain it.
So the canonical "queue → drain → snapshot reads Fresh" round-trip
is not an end-to-end test in the suite; both halves exist, only the
seam is untested.

Critical untested paths:
- The `try_init_embedder` Err arm (already flagged as EH-V1.30.1-8)
  silently returns without clearing dirty flags.
- The `dropped_this_cycle > 0` warn arm (line 139-145) — pairs with
  CQ-V1.30.1-1 P1 fix.
- The println! gating on `cfg.quiet` (line 147) — pairs with
  OB-V1.30.1-9.

**Current shape:** `src/cli/watch/events.rs:131-300+`. Production
function with three production callers and zero tests.

**Proposed direction:**

Build a stub-embedder test harness in `events.rs::tests` (or move
the tests to `cli/watch/mod.rs::tests` if that's where embedder
fixtures already live). Tests to add:

(1) `process_file_changes_zero_files_is_noop` — empty
    `pending_files`, run, assert `state.pending_files.is_empty()`
    and no panics, no embedder init.

(2) `process_file_changes_single_file_drains_into_index` — seed
    one Rust file in a tempdir, push its rel path into
    `pending_files`, run with a stub embedder (existing
    `placeholder_embedding` helper from reconcile tests), assert
    (a) `state.pending_files` is empty after, (b) `store.search`
    returns the new chunk.

(3) `process_file_changes_reports_dropped_warn_once_then_resets`
    — set `state.dropped_this_cycle = 5`, run, assert
    `state.dropped_this_cycle == 0` after (this pins the
    reset-after-warn semantic the audit's CQ-V1.30.1-1 P1 fix
    will fix the *ordering* of).

(4) `process_file_changes_handles_embedder_init_error` — stub
    that returns Err from `try_init_embedder`, assert
    `state.dropped_this_cycle` is preserved (not reset),
    `state.pending_files` is preserved.

The embedder stub probably already exists in
`cli/watch/reconcile.rs::tests` (`placeholder_embedding`). Reuse it.

**Acceptance criteria:**
- Four (or more) direct tests in `events.rs::tests` covering
  the four cases above.
- Coverage for the `try_init_embedder` Err path validated by
  test failures when EH-V1.30.1-8's intended fix lands.
- Tests run in <1s; no real embedder loaded.
- `cargo test --features cuda-index events::` passes.

**Out of scope:**
- Adding production tracing to `process_file_changes` —
  separate observability work (OB-V1.30.1-9 P3).
- Refactoring `process_file_changes` into smaller subfunctions
  (only refactor when tests force it).
- Integration tests that drive the full daemon loop —
  separate scope.
```

## P4-issue: DS-V1.30.1-D3 — Periodic reconcile reads through stale `store` handle
**Why an issue:** Race-condition fix that needs explicit `db_file_identity` check before each periodic reconcile (matching the pattern at `mod.rs:1105`); medium effort since it touches the watch loop's hot path.

**Suggested labels:** `bug`, `tier-3`, `data-integrity`, `concurrency`

**Issue body draft:**
```
DS-V1.30.1-D3: Add db_file_identity check before periodic reconcile fires

The periodic reconcile path (`src/cli/watch/mod.rs:1262-1283`) skips
the `db_file_identity` check that `should_process` does at line 1105.

If `cqs index --force` rotated the DB while the watch loop was idle,
periodic reconcile fires, calls `run_daemon_reconcile(&store, ...)`
against the stale store handle (orphaned inode), and reads
`indexed_file_origins()` + `source_mtime` from the OLD DB. Files that
got newer mtimes in the NEW DB look "stale" against the old store;
reconcile queues them as MODIFIED. The next `should_process` cycle
reopens the store, drains those queued paths, and re-embeds files
that were already current in the new DB.

Worst case isn't corruption — it's silent re-work that defeats
`--force`'s "I just rebuilt cleanly, you can stand down" semantics.

**Current shape:** `src/cli/watch/mod.rs:1262-1283`. The `if reconcile_enabled_flag && ...` block dispatches reconcile without checking
`db_file_identity`.

**Proposed direction:**

Either (a) add a `db_file_identity` check before each periodic
reconcile call and reopen the store if it changed (matching the
pattern at line 1105), or (b) acquire a *shared* (read) index lock
during reconcile so it serializes against `cqs index --force`'s
exclusive lock.

Option (a) is the smaller change.

**Acceptance criteria:**
- A test that simulates `cqs index --force` rotation while
  the watch loop is idle, then triggers periodic reconcile,
  asserts no spurious reindexes happen post-rotation.
- The `db_file_identity` check at line 1105 and the new check
  use the same helper.
- No new lock contention on the hot path (reconcile remains
  read-only against the store).

**Out of scope:**
- Rewriting the watch loop's main scheduling.
- Sharing locks between the watch loop and `cqs index --force`
  beyond the DS-W5 reopen pattern.
- Handling slot promotion races (separate concern, possibly
  DS-V1.30.1-D1).
```

## P4-issue: DS-V1.30.1-D4 — `slot remove` does not check whether daemon is serving the slot
**Why an issue:** Concurrency safety; needs explicit daemon-status probe before unlink and a clear `--force` opt-out path. Medium effort because it touches the slot-remove flow's user-facing error semantics.

**Suggested labels:** `bug`, `tier-3`, `data-integrity`, `concurrency`

**Issue body draft:**
```
DS-V1.30.1-D4: Refuse `slot remove` if a daemon is actively serving the slot

`slot_remove` (`src/cli/commands/infra/slot.rs:322-369`) holds
`acquire_slots_lock` across `read_active_slot → list_slots →
remove_dir_all`, which prevents concurrent CLI invocations from
racing each other. But the lock doesn't bind a *daemon* that's
already serving from `slots/<name>/index.db`.

Concrete failure scenario:
1. `cqs watch --serve --slot foo` is running (holds a long-lived
   `Store::open` against `slots/foo/index.db` + an HNSW Arc + ~500MB
   ONNX session).
2. Operator runs `cqs slot remove foo --force`.
3. The CLI acquires the slots lock (the daemon doesn't hold it —
   it isn't a slot-lifecycle operation), passes the existence
   check, calls `fs::remove_dir_all(&dir)`.

On Linux the unlink succeeds because the daemon's open file
descriptors keep the inodes alive. But:

- The daemon's WAL checkpoint on next write hits an
  unlinked-but-open inode; checkpoints work but the rebuilt HNSW
  persists into the (now-detached) directory tree, which is
  reaped on daemon exit. Hours of incremental rebuild work
  vanish silently.
- The daemon's BatchContext serves stale snapshots forever — no
  path notices the directory disappeared until restart.
- `fs::remove_dir_all` on `index.db-wal` / `index.db-shm` while
  another process is mmap'ing them is undefined per SQLite docs.
  On WSL or any non-overlay FS that surfaces EBUSY or partial
  removal, the user gets a half-deleted slot dir and no rollback.

**Current shape:** `src/cli/commands/infra/slot.rs:322-369`. Calls
`fs::remove_dir_all(&dir)` at line 369 without checking daemon
status.

**Proposed direction:**

Before `remove_dir_all`, check `daemon_status(project_cqs_dir)`
(the same probe `cqs hook status` uses). If a daemon is up *and*
its active slot equals `name`, refuse with a clear error:

```rust
bail!(
    "daemon is currently serving slot '{name}'. \
     Stop it first: systemctl --user stop cqs-watch"
);
```

With `--force`, downgrade to a `tracing::warn!` and proceed
(operator opt-in). Mirrors the existing `--force` semantics for
"this is the active slot" at line 357.

**Acceptance criteria:**
- A test using a `UnixListener` mock daemon socket that responds
  with `slot: "foo"` causes `cqs slot remove foo` to error.
- `cqs slot remove foo --force` against the same mock proceeds
  with a warn.
- The error message points operators at the systemd unit name
  (or `cqs watch` PID) for stopping the daemon.
- Existing slot tests pass.

**Out of scope:**
- Auto-stopping the daemon on remove (operators decide).
- Cross-process atomic-replace of slot dirs (out of scope for
  v1.30.x).
- Windows-side daemon behavior (no Windows daemon today —
  separate work).
```
