## Test Coverage (adversarial)

#### Sparse-vector weight NaN/Inf round-trips through SQLite untested
- **Difficulty:** easy
- **Location:** src/store/sparse.rs:138 — `upsert_sparse_vectors` / `load_all_sparse_vectors:381`
- **Description:** `upsert_sparse_vectors` and `load_all_sparse_vectors` accept and emit `f32` weights with no `is_finite()` guard. SQLite's REAL type happily stores NaN/Inf as a regular bit-pattern, and the loader casts `weight: f64 → as f32` without checking. Existing tests (`test_sparse_roundtrip`, `test_sparse_upsert_replaces`, `test_fk_cascade_removes_sparse_rows_on_chunk_delete`) all use finite, hand-picked weights. A SPLADE encoder hiccup, a corrupted sparse cache row, or an external tool writing into `sparse_vectors` could leak NaN weights, which then propagate through downstream sparse-cosine math (and `f32::NAN > anything` is `false`, so a NaN-weighted chunk silently drops out of every result instead of being clamped or rejected). Mirrors P2-12 (umap NaN, ✅ #1333) but for SPLADE.
- **Suggested fix:** Add two tests in `src/store/sparse.rs` tests module: (1) `upsert_sparse_vectors` with `(token_id, f32::NAN)` and `(token_id, f32::INFINITY)` — pin the contract (reject vs. clamp vs. silently store) and add a runtime guard returning `StoreError::InvalidInput` if reject is the chosen contract; (2) `load_all_sparse_vectors` after a manual `INSERT INTO sparse_vectors VALUES (..., 'nan')` via raw sqlx, to verify the loader either filters or surfaces the row, not silently produces a NaN-weighted vector.

#### `parse_aspx_chunks` malformed/unterminated `<%...%>` block has no test
- **Difficulty:** easy
- **Location:** src/parser/aspx.rs:44 (`CODE_BLOCK_RE`) → src/parser/aspx.rs:141 (`find_code_blocks`)
- **Description:** The CODE_BLOCK_RE pattern `<%(=|:|@|--|--)?(.*?)(--%>|%>)` is non-greedy but on input where the closing `%>` is missing it scans forward through the whole file before failing the alternation. Same DoS shape as `L5K_ROUTINE_BLOCK_RE` (which has SEC-8 acknowledged in comments and an `unterminated_routine_no_panic` test at l5x.rs:782) — but aspx has no equivalent test. Adversarial input: a 10 MB `.aspx` containing many `<%` openers and no closers. The regex crate's linear-time guarantee prevents catastrophic backtracking, but the constant factor on a 50 MB file with 1k unterminated blocks is still measurable.
- **Suggested fix:** Add `parse_aspx_unterminated_code_block_no_panic` and `parse_aspx_truncated_at_open_tag` tests in `src/parser/aspx.rs` mod tests. Feed `"<html><% Response.Write(\"never closes\""` and assert `parse_aspx_chunks` returns `Ok(_)` without panic and within bounded time. Also pin behaviour for `<script runat="server">` with no `</script>` (currently SCRIPT_BLOCK_RE silently drops the unmatched opener — pin that too).

#### `Embedder::split_into_windows` adversarial token boundaries untested
- **Difficulty:** easy
- **Location:** src/embedder/mod.rs:782 — `split_into_windows`
- **Description:** The function has one happy-path test (`split_into_windows_preserves_original_text` at line 2432) and only that one. Untested edges that have explicit handling code:
  (1) `max_tokens == 0` early-returns `Ok(vec![])` (line 788);
  (2) `overlap >= max_tokens / 2` returns an error (line 795);
  (3) `char_end <= char_start` collapse (line 854) — fires on tokens whose offsets are `(0, 0)` for added special tokens; the fallback returns the full text;
  (4) text containing a 4-byte multibyte codepoint at exactly the window boundary so `char_start..char_end` would split mid-codepoint (tokenizer offsets are byte offsets, but Rust string slicing panics on non-char boundaries — relies on the tokenizer never returning intra-codepoint offsets).
  Any of (1)-(4) regressing would be silent for short inputs and only break on exact boundary conditions.
- **Suggested fix:** Add four targeted tests: `split_into_windows_max_tokens_zero_returns_empty`, `split_into_windows_overlap_too_large_errors`, `split_into_windows_collapsed_offsets_falls_back_to_full_text` (synthesise a fake encoding via a mock tokenizer or pick text where padding tokens inject `(0,0)`), `split_into_windows_emoji_at_window_boundary` (emoji-heavy input with `max_tokens` chosen to land the window edge mid-grapheme — assert no panic).

#### `sanitize_fts_query` property tests miss `{` and `}` strip set
- **Difficulty:** easy
- **Location:** src/store/mod.rs:1532 (property tests) vs. src/store/mod.rs:203 (production strip set)
- **Description:** The production `sanitize_fts_query` strips `'"' | '*' | '(' | ')' | '+' | '-' | '^' | ':' | '{' | '}'` (10 chars), but every property test (`prop_sanitize_no_special_chars`, `prop_pipeline_safe`, `prop_sanitize_all_special`, `prop_sanitize_adversarial`) only asserts the first 8 are absent — `{` and `}` are not in the property assertion set or in the `prop::sample::select` input universe. If a refactor accidentally drops `{`/`}` from the strip set, no test fires. FTS5 doesn't error on `{`/`}` literally but they're in the strip set for a reason (column filter syntax `{col}: term`) — losing the strip silently re-enables column-filter injection in a query that was previously sanitized.
- **Suggested fix:** Update the four property tests to include `{` and `}` in (a) the negative-assertion `matches!` set and (b) the `prop::sample::select` adversarial-input vector. One-line edit per test plus a regression test `sanitize_strips_curly_braces` with a hand-picked input like `{path}:foo bar`.

#### Daemon JSON-RPC surface: invalid UTF-8 surrogate halves in args untested
- **Difficulty:** medium
- **Location:** src/cli/watch/adversarial_socket_tests.rs (8 cases) + src/cli/watch/socket.rs:60 — `handle_socket_client`
- **Description:** Existing adversarial tests cover oversized line, trailing garbage, UTF-16 BOM, bare newline, missing command, non-string args, 500 KB arg, and NUL byte. Two remaining adversarial JSON shapes are not pinned: (a) a JSON string containing an unpaired surrogate `"\uD800"` (serde_json 1.0 *accepts* lone surrogates by default and emits a `String` containing `WTF-8`-shaped bytes, which then flows into `dispatch_via_view` — downstream `to_string()` works but any path that crosses an FFI/SQLite boundary may hit issues); (b) deeply-nested JSON like `{"command":"ping","args":[],"x":[[[[[…]]]]]}` (1000 levels) — serde_json default has no recursion limit, can stack-overflow in the parser thread which `catch_unwind` may not catch (SIGSEGV from stack guard page is not a Rust panic).
- **Suggested fix:** Add two tests to `src/cli/watch/adversarial_socket_tests.rs`: `daemon_handles_lone_surrogate_in_string_arg` (assert command is rejected or runs cleanly, no panic, no half-open socket) and `daemon_rejects_deeply_nested_json` (assert: either parser refuses with a structured error, or the daemon thread doesn't take down the whole daemon — handler thread isolation contract). For the recursion case, `serde_json::de::Deserializer::with_recursion_limit(128)` would be the production fix.

#### Daemon socket: zero concurrent-connection / queue-saturation tests
- **Difficulty:** medium
- **Location:** src/cli/watch/socket.rs:45 — `max_concurrent_daemon_clients` and the accept loop in src/cli/watch/daemon.rs
- **Description:** `max_concurrent_daemon_clients()` reads `CQS_DAEMON_MAX_CLIENTS` (default presumably 16-32) and the accept loop is supposed to bound parallel handler threads. Zero tests fire `N+1` simultaneous clients to verify the (N+1)th gets queued, rejected, or admitted — nor that a wedged client (sends partial line, then sleeps) doesn't pin a slot forever (the read_timeout helps but the test doesn't exist). On a daemon servicing N=4 agents this matters: a single hung agent can take down the daemon for the others.
- **Suggested fix:** Add `daemon_caps_concurrent_clients` to `adversarial_socket_tests.rs` — open `max_concurrent + 5` `UnixStream`s, each writing a slow/partial request, and assert that the daemon either queues them, rejects with `too_many_clients`, or honours the read_timeout. Also `daemon_slow_client_does_not_starve_others`: hold one connection open silently, fire a fast valid request through a second connection, verify it completes within 1s.

#### `build_hierarchy` BFS cycle handling untested
- **Difficulty:** easy
- **Location:** src/serve/data.rs:673 — `build_hierarchy`
- **Description:** Existing tests (`build_hierarchy_walks_callees_to_depth`, `hierarchy_extreme_depth_is_clamped`) verify max_depth clamping and basic walk, but not cyclic call graphs (mutual recursion: `a→b, b→a`). The current code uses `depth_by_name.contains_key` to avoid revisiting, which *should* prevent infinite loops, but a regression that switched to "always insert with min depth" would loop forever on cycles. The `serve` HTTP path is exposed to localhost — an attacker on the same host could find a known recursive call pair and DoS the daemon.
- **Suggested fix:** Add `build_hierarchy_handles_mutual_recursion` test in `src/serve/tests.rs` — seed two chunks `a` and `b` with `a→b` and `b→a` call edges, request hierarchy from `a` with `max_depth=10`, assert response contains exactly `{a, b}` with `bfs_depth = {0, 1}` and the call completes in <100ms. Also `build_hierarchy_handles_self_call` (`a→a`).

#### Concurrent writer contention path: zero coverage
- **Difficulty:** hard
- **Location:** src/store/mod.rs:937 (busy_timeout config), src/cli/watch/reindex.rs (writer), src/cli/commands/index/build.rs (writer)
- **Description:** The store relies on SQLite WAL + `busy_timeout(30s)` (recently bumped from 5s in #1450) + an in-process `WRITE_LOCK` mutex to serialise writers. Tests cover concurrent *readers* (`stress_test::test_concurrent_searches`, ignored by default) and verify `busy_timeout_from_env`, but not the actual contention scenario: two writer threads — one running `cqs index` long-batch, one running `cqs notes add` mutation — both reaching `begin_write()`. The interaction between in-process `WRITE_LOCK` and SQLite's BUSY response on the WAL writer lock is the flakiest part of the system on WSL (per memory comments) and has no regression test. A subtle change that drops or shortens the in-process lock could let two `BEGIN IMMEDIATE` collide and surface BUSY beyond the busy_timeout.
- **Suggested fix:** Add `tests/store_concurrent_writers_test.rs` (gated `#[ignore]` if too slow, runnable in CI nightly): spawn 2 threads, each upserting 100 chunks for 5 seconds against a shared `Arc<Store>`, assert both complete with no `database is locked` error and with the union of inserted chunks visible. Then a second test mixing `upsert_chunks_batch` with `upsert_notes_batch` — different write paths must serialise correctly.

#### Embedding pipeline: NaN/Inf escape from `embed_documents` untested
- **Difficulty:** easy
- **Location:** src/embedder/mod.rs:877 — `embed_documents` / `embed_batch:1157`
- **Description:** `Embedding::try_new` rejects NaN/Inf at the constructor, but `embed_batch` constructs Embeddings via `Embedding::new` (the unchecked constructor — see line 121 doc: "**Prefer `try_new()` for untrusted input**"). If ONNX inference produces NaN/Inf for *any* reason (bad ONNX op, dim-zero quirk, cuDNN driver bug, `--features ep-coreml` weirdness), the Embedding sails through to HNSW insert, where it pollutes the search index. The `cache.rs` writer at line 639 *does* check `!f.is_finite()` before writing to disk cache — but the in-memory hot path doesn't. Tests cover `try_new` rejection but not "embed_documents output is finite" — no test confirms inference output finiteness.
- **Suggested fix:** Add `embed_documents_output_is_finite` and `embed_query_output_is_finite` integration tests in `tests/embedding_test.rs` — call against a fixed mock model and assert `result.iter().all(|e| e.as_slice().iter().all(|v| v.is_finite()))`. Cheap regression-catcher; would have caught any future ONNX-runtime upgrade that started leaking subnormals as NaN.

#### `serve` HTTP handlers: unicode/zero-width/control chars in `chunk_id` path param
- **Difficulty:** medium
- **Location:** src/serve/handlers.rs:172 (`chunk_detail`), src/serve/handlers.rs:244 (`hierarchy`)
- **Description:** Chunk IDs flow from a URL `Path(id): Path<String>` straight into SQL `WHERE id = ?` (parameterised — no SQL-injection risk) but no test pins behaviour for adversarial path parameters: zero-width joiner (`‍`), RTL override (`‮`), NUL byte (`%00`), 100 KB id (URL-encoded), URL-encoded `../../etc/passwd`, percent-decoded surrogate. axum/tower decode the path before it lands in the handler; a refactor swapping `Path<String>` for `Path<RawString>` or adding any post-decode normalisation would silently change behaviour. Existing `chunk_detail_unknown_id_returns_404` only tests a normal-ASCII unknown id. Adversarial IDs in tracing logs are also a concern — the chunk_id is logged at info level (`tracing::info!(chunk_id = %id, "serve::chunk_detail")`) and an RTL override there can flip log lines in journalctl.
- **Suggested fix:** Add `chunk_detail_handles_adversarial_unicode_id` and `hierarchy_handles_oversized_id_path` tests in `src/serve/tests.rs`. Each fires an `axum::test::TestRequest` with the adversarial id and asserts: HTTP 404 (or 400 for clearly-malformed) with no panic, log line is bounded length, no half-open response. Also add a 64 KB id test to pin "what's the URL length cap" (axum's default is server-config-dependent).

#### `parse_env_f32` rejects NaN but `parse_env_usize_clamped` zero-input UB untested
- **Difficulty:** easy
- **Location:** src/limits.rs:269 — `parse_env_usize_clamped`
- **Description:** Existing tests cover above-max, below-min, garbage, and missing — but the docstring says "Missing/zero/garbage falls back to `default` (also clamped)", and the implementation at line 273 reads `Ok(n) if n > 0 => clamp(n)` — meaning `n=0` falls through to the `_ =>` arm, which `clamp(default)`. If a future refactor moves the `n > 0` check, a caller passing `min=1, max=100, default=0` would silently get 0 (not clamped to min=1), which would then cause divide-by-zero in `embed_batch_size` math. Worth pinning given how widely this helper is used (RT-RES limits, sparse batch sizes, daemon timeouts).
- **Suggested fix:** Add `parse_env_usize_clamped_zero_input_uses_clamped_default` test asserting `parse_env_usize_clamped("CQS_TEST_KEY_DOES_NOT_EXIST", 0, 1, 100) == 1` (not 0) — which forces the `default.clamp(1, 100)` branch to actually fire. Also `parse_env_usize_clamped_default_below_min` to pin the contract on misconfigured-default callers.

DONE
