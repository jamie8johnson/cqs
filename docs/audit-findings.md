# Audit Findings — v1.46.1

Audit date: 2026-06-15
Source: 16-category discovery fan-out, 22 raw findings → 19 triaged rows after dedup/cluster.
Triage: `docs/audit-triage.md`. Prior cycle archived at `docs/audit-triage-v1.42.0.md` (P1+P2 complete; deduped against — no new finding re-states a closed v1.42/v1.40 item).

## Resource Management

#### Daemon dispatch catch_unwind is dead under `panic = "abort"` — one bad request aborts the warmed daemon

- Difficulty: medium · Impact: high
- Location: src/cli/watch/socket.rs:257-330 (catch_unwind) × Cargo.toml:355 (`[profile.release] panic = "abort"`)
- Description: `handle_socket_client` wraps dispatch in `std::panic::catch_unwind` with an `Err(payload)` arm that writes "internal error (panic in dispatch)" and logs "Daemon query panicked — daemon continues" — the daemon's stated panic-survival contract. But the shipped artifact is the release profile, which sets `panic = "abort"` (confirmed Cargo.toml:355). Under abort there is no unwinding: the panic hook runs and the process terminates *before* `catch_unwind` can intercept. In the systemd `cqs-watch` daemon (release binary), the catch_unwind is dead code: any panic on the dispatch path (an `.expect()`/`unwrap()`/slice-index/overflow on an edge-case or adversarial request) aborts the whole daemon, dropping ~500MB of warmed ONNX sessions plus HNSW/SPLADE/call-graph caches. systemd restarts cold; the next query pays full model-init + index-rebuild latency. A client that can reliably trip a dispatch panic has a remote restart/DoS primitive. Any test for this behavior silently asserts nothing under release.
- Suggested fix:
  - Decide the panic policy: (a) set `panic = "unwind"` for the binary so the existing catch_unwind isolates dispatch panics (accept the LTO/size cost, or scope abort to non-daemon builds), OR (b) keep abort and DELETE the catch_unwind + Err arm + the misleading "daemon continues" log, replacing with an honest "a dispatch panic aborts the daemon; systemd restarts it" note plus a defensive sweep of the dispatch path's panic sources.
  - Add a daemon integration test that sends a request known to panic a handler and asserts either the panic envelope (option a) or that the panic sources are removed (option b).
  - This is a judgment call (panic-policy tradeoff vs. defensive sweep) — route to an issue, not an auto-fix.

#### Daemon `in_flight` slot decremented post-call, not via RAII — leaks a connection slot on panic outside the inner catch_unwind

- Difficulty: easy · Impact: medium
- Location: src/cli/watch/daemon.rs:246-247
- Description: The accept loop bumps `in_flight` (fetch_add) then spawns a thread whose body is `handle_socket_client(stream, &ctx_clone); let prev = in_flight_clone.fetch_sub(1, …)`. The decrement only runs if `handle_socket_client` returns. The inner `catch_unwind` in socket.rs:257 covers only the dispatch closure — the region before it (set_read/write_timeout, BufReader `read_line` over attacker-controlled bytes, `serde_json::from_str`, the arg-validation walk, the early `write_daemon_error_tracked` calls) and the write helpers called *after* the catch_unwind are all outside it. A panic there unwinds past the `fetch_sub`, leaking the slot permanently — the spawned thread has no Drop-based release. Accumulated leaks raise `in_flight` toward `max_clients`, after which the accept loop rejects every new connection ("daemon busy"), bricking the daemon with no clients connected. Moot under the current `panic = "abort"` (process aborts), but live the moment the panic policy changes to unwind (the natural fix for the catch_unwind finding) and already live in any debug/test/unwind build.
- Suggested fix:
  - Replace the post-call `fetch_sub` with an RAII guard struct holding `Arc<AtomicUsize>` that does `fetch_sub(1, AcqRel)` in `Drop`, constructed right after the `fetch_add` and moved into the closure. Decrement then runs on normal return, early return, and unwind alike. Subsumes the manual decrement on the spawn-failure path.
  - Add a test that panics inside `handle_socket_client` (unwind build) and asserts `in_flight` returns to its prior value.

#### Overlay delta builds full records/masked_origins/parse_set BEFORE the size cap rejects — cap is post-hoc

- Difficulty: easy · Impact: low
- Location: src/worktree_overlay.rs:809-868 (discover_delta)
- Description: `discover_delta` parses the entire `git diff --name-status` output into `records`, appends every untracked file, builds `masked_origins` (HashSet) and `parse_set` (Vec), and ONLY THEN checks `overlay_max_files()` (line ~863). The cap is documented as a DoS rail ("overlay delta exceeds cap — skipping") but is enforced after all three collections are already fully materialized. A worktree with a huge number of changed/untracked files — the exact case the cap names — still pays the full O(N) allocation before being rejected; the rail doesn't bound peak memory of the function it guards. Runs on every overlay cache miss in a worktree checkout. Low impact because git output bounds it in practice.
- Suggested fix:
  - Check delta size against `overlay_max_files()` as records accumulate (after tracked records, and again while folding untracked entries), bailing with DeltaTooLarge before continuing to build masked_origins/parse_set. Keep the final check as a backstop.

## Test Coverage (happy path)

#### apply_dead_overlay Direction A (parent-dead → live resurrection) is untested at every layer

- Difficulty: medium · Impact: high
- Location: src/store/calls/dead_code.rs:1022-1031 (Direction A); overlay_has_real_caller at :934; only test src/cli/worktree_overlay_build.rs:517
- Description: `apply_dead_overlay` drives the worktree-overlay dead-code merge for `cqs dead`, `cqs ci`, and `cqs review`. It has two directions: B (parent-live → dead, well-tested) and A (parent-dead → live: `confident/possibly_pub.retain(|d| !overlay_has_real_caller(...))` drops a previously-dead function the worktree now really-calls). The ONLY test exercising apply_dead_overlay seeds a previously-LIVE function whose caller is removed — purely Direction B. Zero coverage of the resurrection path: no test where a parent-dead function gains a real worktree caller and is dropped from the dead set; zero coverage of `overlay_has_real_caller` / the retain predicate / the `participated=true` flag flip. This path is freshly churned (#1942/#1943/#1957). A regression in `overlay_has_real_caller` or the retain predicate would falsely keep a now-live function flagged dead — a high-visibility false positive in the exact surface agents query — and a green suite would not catch it because every fixture is born without an overlay-added caller.
- Suggested fix:
  - Add an inline test (mirroring the existing Direction-B one) that seeds a genuinely-dead parent function (zero callers), builds a worktree overlay whose delta adds a real call edge to it, runs dead_overlay with `Some(&overlay)`, and asserts (a) the function is absent from `output.dead` and (b) `participated == true`.
  - Add a second case asserting `overlay_has_real_caller` returns false for a `doc_reference`/`macro_heuristic` edge so a non-real overlay edge does NOT resurrect.

#### distinct_callees_from_origins (masked-origin candidate enumeration) has no direct test for multi-origin / dedup behavior

- Difficulty: easy · Impact: medium
- Location: src/store/calls/query.rs:684 (distinct_callees_from_origins); sole production caller src/store/calls/dead_code.rs:1042
- Description: `distinct_callees_from_origins(&masked)` feeds the entire Direction-B candidate set for the dead-overlay merge — it enumerates the distinct callee names the masked (delta-origin) files used to call, parent-side. Zero references in tests/ (grep-confirmed); reached only transitively through the single Direction-B inline test, which seeds exactly one masked origin and one candidate. Its DISTINCT-ness and multi-origin union behavior (two masked files with overlapping callee sets must dedup; empty masked list must yield empty) are never asserted. A bug here (dropping DISTINCT, or wrong path normalization at dead_code.rs:1040) would silently shrink or duplicate the Direction-B candidate set, missing or doubling dead-code flips under overlay.
- Suggested fix:
  - Add a focused unit: seed call edges from two origin files with overlapping and distinct callees, call `distinct_callees_from_origins` with both, assert the returned set is the deduped union; add an empty-origins case (empty result) and a path-normalization case (non-normalized separator still matches).

#### Dart body-inclusive canonical_hash (the extra-node comment-stripping path) is untested

- Difficulty: medium · Impact: low
- Location: src/parser/chunk.rs:178-192 (canonical_hash_spanning, the Option<extra> branch :190-192); production call :412; tests at :1152 and tests/language_test.rs:7656
- Description: canonical_hash's non-comment-stripping invariant is well covered, but every existing test calls it with `extra = None`. The `extra` (body) node parameter, added in #1970 for Dart body-inclusive chunking, collects comment ranges from a SECOND node so a comment-only edit inside a Dart method body does not change the chunk's canonical_hash (the chunk id / embedding-cache key — the injectivity surface that produced #1947). No test passes a non-None extra; `test_dart_chunk_includes_body` only asserts body text is included, never canonical_hash stability across a body-internal comment change. The merge/sort/splice of comment ranges spanning two disjoint nodes — the only thing the extra branch adds — has zero assertion. A regression (base offset from the wrong node, ranges not merged) would change Dart chunk ids on comment-only edits, defeating cache reuse, with no failing test.
- Suggested fix:
  - Add a Dart case to canonical_hash_tests asserting that inserting a `//` or `/* */` comment inside a Dart method/function *body* leaves canonical_hash unchanged, and a real body code change *does* change it — parsing through Parser so the real body node feeds the `extra` argument.

## Test Coverage (adversarial)

#### L5X/L5K call-graph + type-ref extraction (parse_l5x_all) has zero test coverage, including over malformed/error-recovered ST

- Difficulty: easy · Impact: low
- Location: src/parser/l5x.rs:590-613 (parse_l5x_all), :628-652 (extract_chunk_type_refs); tests :1242 and :1277 discard `_calls`/`_types`
- Description: The production path `parse_l5x_all` derives `all_calls` and `all_types` for the call/type graph. Both production-path tests bind the relationship outputs as `_calls`/`_types` and never assert on them. The chunk-extraction side is well covered adversarially (unclosed CDATA, unterminated ROUTINE, CRLF, overflow saturation), but the call/type-ref derivation over ST content — particularly ST that tree-sitter error-recovers on, where `extract_types`/`extract_calls_from_chunk` walk a partial tree — is entirely unexercised. The code degrades gracefully (warn + empty Vec) but there is no guard that malformed ST yields no panic and no garbage edges, nor that valid ST with a routine-call produces the expected edge.
- Suggested fix:
  - Add a happy-path test: `parse_l5x_all` on an L5X with ST containing a function-block/routine invocation, assert non-empty `all_calls`/`all_types` with the expected edge.
  - Add an adversarial test: feed syntactically-broken ST and assert `parse_l5x_all` returns Ok with no panic and no malformed entries (exercises the error-recovery branches).

## Performance

#### embed_batch deep-clones every chunk string per batch (texts.to_vec()) — comment wrongly claims it's unavoidable

- Difficulty: easy · Impact: medium
- Location: src/embedder/core.rs:1006-1011
- Description: `embed_batch(&self, texts: &[String])` calls `self.tokenizer()?.encode_batch(texts.to_vec(), true)`. The inline comment asserts `texts.to_vec()` is "unavoidable — the tokenizer API does not accept &[impl AsRef<str>]". False: tokenizers 0.23 `encode_batch<E: Into<EncodeInput<'s>>>` accepts `Vec<E>` and `&'s str: Into<EncodeInput<'s>>`. So `texts.to_vec()` clones every String *body* (chunk content, often KB each) into a fresh Vec on every batch, purely to feed the tokenizer. This is the embed/index hot path: ~16k chunks at batch_size 64 ≈ 260 batches, each deep-copying up to 64 content strings dropped immediately after `encode_batch` consumes them. The sibling `token_counts_batch` does `texts.to_vec()` on `&[&str]` (cheap pointers, fine); only the `&[String]` site deep-clones. Distinct from the closed PERF-V1.42-5 (which fixed the per-chunk tokenizer-vocab clone in split_into_windows).
- Suggested fix:
  - Replace `texts.to_vec()` with `texts.iter().map(String::as_str).collect::<Vec<&str>>()`. Fix or delete the inaccurate "unavoidable" comment.
  - Optionally switch to `encode_batch_fast` since cqs reads only ids/mask, not char offsets.

#### SPLADE encode_batch tokenizes serially in a map() loop, forgoing the tokenizer's built-in rayon parallel batch

- Difficulty: easy · Impact: medium
- Location: src/splade/mod.rs:799-808
- Description: `SpladeEncoder::encode_batch` tokenizes one input at a time: `non_empty_texts.iter().map(|t| tokenizer.encode(*t, true)).collect()`. The HF `Tokenizer` exposes `encode_batch`, which tokenizes the whole batch in parallel via rayon. The serial per-item loop runs single-threaded tokenization for the entire SPLADE batch on every reindex batch. On the production index-build path (index/build.rs:873, watch/reindex.rs:165). Downstream reads only ids/attention_mask, so `encode_batch` is a drop-in — no char-offset or per-item-error dependency blocks it.
- Suggested fix:
  - Replace the serial map with `tokenizer.encode_batch(non_empty_texts, true)` (or `encode_batch_fast`). Map the single batch-level Result to SpladeError once.

#### CallGraph::edge_meta allocates two Arc<str> per lookup in cross-project caller/callee rendering loops

- Difficulty: easy · Impact: low
- Location: src/store/helpers/types.rs:568-573 (called from src/store/calls/cross_project.rs:345,394)
- Description: `edge_meta(&self, caller: &str, callee: &str)` does `self.edges.get(&(Arc::from(caller), Arc::from(callee)))` — building two fresh heap `Arc<str>` solely to form the tuple HashMap key, then dropping them. Called once per caller (cross_project.rs:345) and per callee (:394), i.e. per rendered edge. Result sets are bounded by direct callers/callees of one symbol, so impact is low, but it's a gratuitous double allocation on every edge during cross-project caller/callee/impact rendering.
- Suggested fix:
  - Make the lookup borrow-key friendly (a `&(&str, &str)` Borrow shim), nest the HashMap, or intern caller/callee once at the call site and pass the Arcs in.

## Documentation

#### README documents fictional CQS_CACHE_MAX_BYTES env var with inverted behavior; real knob is CQS_CACHE_MAX_SIZE

- Difficulty: easy · Impact: medium
- Location: README.md:938 vs src/cache/embedding_cache.rs:169, :421-488
- Description: README:938 documents `CQS_CACHE_MAX_BYTES` as "(unset) | Soft cap; emits tracing::warn! … Does NOT auto-prune". That env var does not exist in src/ (grep: zero hits). The actual embeddings-cache size knob is `CQS_CACHE_MAX_SIZE` (default 1 GB, read at embedding_cache.rs:169) and it DOES auto-evict (`evict_if_needed` / "Evict oldest entries if cache exceeds max size", loop at :483-488). The README entry is triple-wrong: nonexistent name, wrong default (unset vs 1 GB), inverted behavior (warn-only/no-prune vs eviction). The real knob is also documented correctly at README:778, so an agent setting CQS_CACHE_MAX_BYTES to bound growth gets a silent no-op. Docs-lying-is-P1 class for an agent-facing config surface.
- Suggested fix:
  - Delete the README:938 `CQS_CACHE_MAX_BYTES` row. The real cap is already at README:778. If a warn-only soft-cap was intended but never built, file it separately rather than documenting it as live.

#### README documents nonexistent CQS_CONVERT_WEBHELP_BYTES override; merged-output cap is a hardcoded 50 MB const

- Difficulty: easy · Impact: medium
- Location: README.md:803 vs src/convert/webhelp.rs:118, :168
- Description: README:803 documents `CQS_CONVERT_WEBHELP_BYTES` (default 52428800 / 50 MiB) as the configurable merged-output cap with truncate-with-warn. The env var does not exist (grep: zero hits). The cap is the hardcoded const `MAX_WEBHELP_BYTES = 50 * 1024 * 1024` (webhelp.rs:118), enforced at :168. The truncate-with-warn behavior is real but NOT configurable via the documented var; webhelp.rs honors `CQS_CONVERT_MAX_PAGES` and `CQS_CONVERT_PAGE_BYTES`, not this one. An agent trying to lift the cap gets a silent no-op.
- Suggested fix:
  - Replace the README:803 row with a note that the merged-output cap is a fixed 50 MB (src/convert/webhelp.rs:118) and point to the real knobs `CQS_CONVERT_MAX_PAGES` / `CQS_CONVERT_PAGE_BYTES`. (If configurability is later desired, wire MAX_WEBHELP_BYTES through crate::limits and restore the doc.)

#### README documents wrong default for CQS_BUSY_TIMEOUT_MS (5000); actual fallback is 30000

- Difficulty: easy · Impact: low
- Location: README.md (CQS_BUSY_TIMEOUT_MS row) vs src/store/mod.rs:1075,:1352 and src/store/helpers/sql.rs:15
- Description: README documents `CQS_BUSY_TIMEOUT_MS | 5000 | SQLite busy timeout`. The actual fallback when unset is 30000 ms. All production paths call `busy_timeout_from_env(30_000)`; the only non-30000 literals are tests (1234, 500, 0). The source-of-truth comment at store/mod.rs:54 says "30s"; cache/mod.rs:170 documents 30 s embedding / 15 s query defaults. No code path uses 5000 for this var. An operator reasoning about lock contention from the README under-estimates the default wait window 6×.
- Suggested fix:
  - Change the README default from `5000` to `30000` (30 s). Optionally note the cache pools use context-specific defaults (30 s embedding / 15 s query) per cache/mod.rs:170.

## Observability

#### InvalidData (non-UTF8) silent-skip in parse_file_relationships_with_candidates diverges from the two sibling parse paths that warn

- Difficulty: easy · Impact: low
- Location: src/parser/calls.rs:1284-1285
- Description: The Lane-2 candidate-edges relationship path (#1934), on a non-UTF8 read (`ErrorKind::InvalidData`), silently returns empty relationships with NO log. Both other InvalidData sites in the parser warn for exactly this case: src/parser/mod.rs:242 and :505 (`tracing::warn!(path = …, "Skipping non-UTF8 file")`). A sweep straggler — N-1 of N sites carry the warn. Currently LOW impact because this family has only test callers today (production routes through `parse_file_all_with_chunk_calls` → the mod.rs:505 site that warns), but these are public production-grade parser APIs; the silent branch ships the moment any production code adopts them. Shares its root file with the test-only-parse-path finding below.
- Suggested fix:
  - Add `tracing::warn!(path = %path.display(), "Skipping non-UTF8 file");` to the InvalidData arm at calls.rs:1284 before the return, mirroring mod.rs:242/:505 verbatim.
  - Optionally add a completeness guard asserting all three InvalidData arms emit the warn.

## Code Quality

#### Test-only parallel call/type-extraction path diverges from the production index path

- Difficulty: medium · Impact: medium
- Location: src/parser/calls.rs:1258-1569 (parse_file_relationships_with_candidates) vs production src/parser/mod.rs:478-568 (parse_file_all_inner)
- Description: `parse_file_calls` → `parse_file_relationships` → `parse_file_relationships_with_candidates` (~310 lines re-implementing call + type + candidate extraction) is reached ONLY from tests (`cqs callers parse_file_calls` returns total=10, all `#[test]` fns; the production-looking crud.rs:2122 caller is inside a `#[test]`). The live index path is `parse_file_all_with_chunk_calls` → `parse_file_all_inner`, a SEPARATE copy of the same logic. This is the structural fault behind #1958/#1955 (the L5X ST extractor passed its tests through one entry while production used another). Two concrete divergences remain: (1) grammar-less dispatch order differs — the test path consults `custom_call_parser` THEN `custom_all_parser` THEN markdown (calls.rs:1306-1318), while production consults ONLY `custom_all_parser` THEN markdown (mod.rs:533-545), so a future language setting `custom_call_parser` (field exists, currently None everywhere) would be honored in tests but silently ignored in production; (2) the chunk-calls comment at mod.rs:529 says "uses per-chunk extract_calls_from_chunk" but the code calls extract_calls_and_candidates_from_chunk. A green suite over the test-only copy gives false confidence the production copy matches.
- Suggested fix:
  - Collapse to one extraction path: have the test-only `parse_file_relationships*` family delegate to `parse_file_all_inner` (call `parse_file_all_with_chunk_calls` and project to (calls,types)), or delete the parallel path and point its tests at the production entry.
  - At minimum, factor the grammar-less dispatch (`if grammar.is_none()` block) into one shared helper so the two sites cannot drift, and add a guard test asserting a language with `custom_call_parser=Some` is honored on the production index path.
  - Architecture call (collapse-vs-delegate-vs-factor) — route to an issue.

## API Design

#### Two parallel `*Args` layers (clap wire vs core) with no exhaustiveness guard; `include_types`/`type_impact` is the live symptom

- Difficulty: medium · Impact: medium · existing issue: 1459
- Location: core src/cli/commands/graph/impact.rs:42 (`ImpactArgs.include_types`) vs wire src/cli/args.rs:341 (`type_impact`); systemic pattern in src/cli/batch/handlers/search.rs:115-154, src/cli/commands/search/query.rs:255-294, etc.
- Description: Two merged findings, same root (issue #1459). (Symptom) The same boolean has three names: CLI flag `--type-impact`, wire field `type_impact`, core field `include_types`. The core struct is `#[derive(Deserialize)] #[serde(default)]` and documented as the MCP-ready surface but lacks `deny_unknown_fields`, so a consumer deserializing the core `ImpactArgs` and sending `{"type_impact":true}` (the natural wire name) silently gets `include_types:false` — type-impact analysis quietly skipped. Every other core field in this family mirrors its CLI flag; `include_types` is the lone exception, and it affects a documented deserialization contract. (Root) Every command-core command maintains TWO Args structs plus ≥2 hand-written field-by-field copy functions (CLI adapter + daemon `dispatch_*` adapter). For `search` there are four core-`QueryArgs` constructors each enumerating ~24 fields. No compiler-enforced exhaustiveness: adding a field to the WIRE struct and forgetting to thread it into `daemon_query_args` compiles cleanly and silently drops the knob on the daemon path. The "Daemon-Path is a Parallel Surface" / "Wiring Verification" lessons document this exact drift class. Parity tests catch only fields a test exercises.
- Suggested fix:
  - Rename core `ImpactArgs.include_types` → `type_impact` (or add `#[serde(rename = "type_impact")]`) and add `#[serde(deny_unknown_fields)]` to the core Args so a name mismatch errors instead of silently defaulting.
  - Reduce systemic duplication: a single shared `From<&args::SearchArgs> for QueryArgs` (or `core_args()` method) so there is ONE field enumeration; `..Default::default()` only for genuinely surface-specific fields; extend parity tests with a field-driving round-trip or a sweep guard on the field-set correspondence.
  - Tracked under #1459 — file the rename+deny_unknown_fields slice if a new sub-issue is wanted; otherwise carry on #1459.

#### SearchArgs duplicates the three overlay fields inline instead of flattening the shared OverlayArgs struct

- Difficulty: easy · Impact: low
- Location: src/cli/args.rs:257-284 (SearchArgs inline overlay/no_overlay/overlay_root) vs shared OverlayArgs at args.rs:103-129
- Description: `OverlayArgs` was introduced as "the shared subset for the seed-overlaid graph-adjacent commands" and is flattened by GatherArgs/ImpactArgs/ScoutArgs/DeadArgs/CallersArgs. SearchArgs instead hand-declares the identical three fields plus near-duplicate doc comments. The copies have already begun to drift — the SearchArgs copy carries an extra `skipped-no-daemon` paragraph OverlayArgs lacks. The clap `#[arg]` attributes are otherwise identical, so the flatten is a drop-in.
- Suggested fix:
  - Replace the three inline fields with `#[command(flatten)] pub overlay: OverlayArgs`; update accesses (`args.overlay` → `args.overlay.overlay`, etc.) at the call sites (daemon_overlay_active in batch/handlers/search.rs:161-167, prepare_overlay_request).
  - Fold the search-specific `skipped-no-daemon` note into the shared OverlayArgs doc so all flatteners describe the no-daemon degradation uniformly.

## Error Handling

#### Overlay fingerprint collapses transient read errors to the deletion sentinel (ZERO32), risking a stale-overlay cache hit

- Difficulty: medium · Impact: low
- Location: src/worktree_overlay.rs:978-981 (fingerprint), content_digest at :1003-1014
- Description: `fingerprint()` is the overlay LRU cache's identity key. For a non-Deleted DeltaRecord it streams the worktree file through blake3, but on failure does `Err(_) => hasher.update(&ZERO32)` — the SAME 32-zero sentinel a `Deleted` record contributes. `content_digest` returns Err for ANY of: a symlink, ENOENT (replaced mid-read), a permission flap, a transient I/O error — all distinct from "deleted", all collapsing to the same preimage. If a Modified file is momentarily unreadable during a fingerprint recompute (an editor's atomic rename-replace racing the digest read), it hashes as ZERO32 instead of its real content; if a previously-cached overlay shares that ZERO32 contribution for the same record set, the LRU returns a stale overlay built from different content — incorrect results served as a cache hit, with no warning (the Err arm is silent, unlike the rest of the module). The doc comment describes only the deletion sentinel; the error case riding the same value is undocumented and unlogged.
- Suggested fix:
  - On `Err(e)` log a warn and feed a DISTINCT sentinel (a domain-separated tag byte before ZERO32, or blake3 of the error-kind discriminant) so an unreadable-modified file can never share a fingerprint with a clean cached overlay.
  - Alternatively, propagate the error out of `fingerprint()` (return Result) so the caller treats a fingerprint failure as "force rebuild, never trust cache".

## Platform Behavior

#### platform_cfg_sweep_test guards only the unsized-binding shape, not the dead-code-on-a-sibling-target shape that also broke v1.46.0

- Difficulty: medium · Impact: medium
- Location: tests/platform_cfg_sweep_test.rs (whole file); exemplars src/store/mod.rs:411 (SLOW_MMAP_FSTYPES), src/cli/commands/serve.rs:122 (url_safe_for_cmd)
- Description: The v1.46.1 release build (e2c67c10) fixed THREE cross-target breakages: (1) the E0277 unsized-`Path` binding in `BatchView::resolve_overlay`, and (2)+(3) two `dead_code`-on-a-sibling-target warnings (`url_safe_for_cmd` dead on macOS; the `SLOW_MMAP_FSTYPES`/`fstype_for_path`/`path_starts_with` cluster dead on a Windows release build). Both classes fire only on the tagged cross-build under `-D warnings`, never on Linux PR CI (ci.yml is ubuntu-only). The new `platform_cfg_sweep_test.rs` only forward-scans shape (1) (`no_unannotated_linux_only_unix_let_block`). Shape (2)/(3) — an item whose ONLY callers live inside a single-OS `cfg` block but the item itself is ungated, so it's dead on every other target — has NO guard. The same incomplete-sweep class the test was created to defend remains structurally invisible at PR time for its other two members.
- Suggested fix:
  - Add a second forward scan (or clippy config) flagging ungated `fn`/`const`/`static` items reachable only from single-target `cfg` arms; OR, pragmatically, run `cargo clippy --target x86_64-pc-windows-msvc` and `--target aarch64-apple-darwin` with `-D warnings` on a scratch/check basis in CI (the project already documents `cargo check --target <sibling>` as the verification method).
  - Test-infra/CI design call (lint-scan vs cross-target check job) — route to an issue.

#### is_wsl_drvfs_path cfg!(windows) UNC branch is effectively dead — forward-slash literals vs backslash Windows paths

- Difficulty: easy · Impact: low · existing issue: 1512
- Location: src/config.rs:136-140 (is_wsl_drvfs_path)
- Description: The UNC arm is `if (is_wsl() || cfg!(windows)) && (s.starts_with("//wsl.localhost/") || s.starts_with("//wsl$/"))`. The `cfg!(windows)` disjunct was added to fire on native Windows (PB-V1.40-6 / #1779), but the match uses forward slashes. On native Windows `path.to_str()` returns backslash-separated paths — a WSL UNC share is `\\wsl.localhost\Ubuntu\…`, never `//wsl.localhost/…` — so the test can only succeed when paths use `/` (a Linux/WSL view), where `is_wsl()` already covers it. The `cfg!(windows)` disjunct contributes nothing: dead on the one platform it was added for. Consequence: a native-Windows user editing on a `\\wsl.localhost\` share gets `is_wsl_drvfs_path == false`, so `coarse_fs_resolution` falls through to the `not(any(linux,macos))` arm returning `Duration::ZERO` instead of 2 s — the watch loop's mtime-equality skip can silently drop a rapid re-save. Niche (native-Windows watch over a WSL UNC share); the broader Windows ZERO-return is already on #1512 (PL-V1.38-9). Distinct from the closed PB-V1.40-6 (which guarded the arm for non-WSL Linux hosts).
- Suggested fix:
  - Normalize separators before the UNC check: `let norm = s.replace('\\', "/");` then match against `//wsl.localhost/` / `//wsl$/`; or add explicit backslash variants under the `cfg!(windows)` disjunct.
  - Fold into the #1512 Windows-native FS-detection work (PL-V1.38-9).

## Extensibility

#### cqs dead known-gap allowlists are compile-time consts despite self-describing as extensible

- Difficulty: easy · Impact: low
- Location: src/cli/commands/review/dead.rs:98 (SERVED_ASSET_PREFIXES), :144-176 (EXTERNAL_TRAIT_METHODS), :110-113 (is_python_dunder)
- Description: The `cqs dead` known-gap classifier hardcodes framework-dispatch conventions as `const` slices/predicates with no config seam, yet the doc comment at :93-94 claims "The prefix table is extensible so other corpora can add their served-assets roots" — but it is a `const`, so "extensible" requires recompiling. No config field for any of these (grep of config.rs: nothing). For a non-cqs corpus this means false `dead` verdicts (assets under `public/`/`static/`/`assets/`; framework dispatch through traits not in the list — Drop/Default/Iterator/axum/clap-derived) with no extension path. The serde Visitor list is also incomplete even as a fixed set. The single-user stance caps practical impact (these are tuned for cqs's own corpus), but the asymmetry is real: embedder/reranker selection IS config/env-driven, while these allowlists are the lone classification surface with a hardcoded-only story that contradicts its own docstring.
- Suggested fix:
  - Either (a) wire a `[dead]` config section (`served_asset_prefixes`, `external_trait_methods`, `python_dunder`) merged with built-in defaults, matching the embedder/reranker precedence pattern; OR (b) drop the "extensible" language from the docstring (:93-94) so the doc stops promising behavior the const doesn't deliver.
  - If keeping it hardcoded, at minimum complete the serde Visitor surface in EXTERNAL_TRAIT_METHODS for cqs's own correctness.
