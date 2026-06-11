## Observability

#### `search_filtered` emits duplicate nested `info_span!("search_filtered", ...)`
- **Difficulty:** easy
- **Location:** src/search/query.rs:104 and src/search/query.rs:136
- **Description:** The public `pub fn search_filtered` at line 96 enters `info_span!("search_filtered", limit, threshold, rrf)` at line 104, then inside its body calls private `search_filtered_with_notes` at line 128 which enters *another* `info_span!("search_filtered", limit, rrf)` at line 136. Every user-facing search produces a span tree with two same-named "search_filtered" spans nested one inside the other. This pollutes flame graphs (the inner "search_filtered" span looks like recursion that isn't there), inflates the `tracing::Span` allocation count for every query (the hottest path in the daemon), and makes `journalctl` correlation noisier — operators see `search_filtered` twice per query without realising the inner one is redundant. Two callers exist for `search_filtered_with_notes`: `search_filtered` (which just entered) and `search_filtered_with_index` (path 689 enters its own `search_index_guided` span first, so a triple-nested duplicate fires there).
- **Suggested fix:** Drop the inner `info_span!` at line 136 (the outer one already covers the work) — or rename it to `search_filtered_inner` so the names disambiguate. Same review for `search_filtered_with_index` (line 667) which also delegates into `search_filtered_with_notes`.

#### `verify_hnsw_checksums` lacks an entry span on the integrity-check hot path
- **Difficulty:** easy
- **Location:** src/hnsw/persist.rs:121
- **Description:** `verify_hnsw_checksums` walks every file in the HNSW manifest, opens it, and BLAKE3-hashes the contents (potentially hundreds of MB). The function emits three `tracing::warn!` lines on IO error and returns an Err on mismatch, but it never opens an `info_span!`. Operators investigating "why is `cqs index` slow on startup" or "why did load fail" have no way to time the verify pass or see how many files it processed — a startup with a 500 MB HNSW blob spends seconds here invisibly. DS-V1.33-3 just hardened the missing-file branch (P1 fix) but didn't add the span. This sits adjacent to `pub fn save` and `pub fn load_with_dim`, both of which already have `info_span!`.
- **Suggested fix:** Add `let _span = tracing::info_span!("verify_hnsw_checksums", dir = %dir.display(), basename).entered();` at the top of the function. On Ok, emit a completion event with `files_verified` count and `elapsed_ms`.

#### `validate_summary` and `detect_all_injection_patterns` run untraced on every LLM result
- **Difficulty:** easy
- **Location:** src/llm/validation.rs:87, src/llm/validation.rs:152
- **Description:** Both functions run on every batch result the LLM provider returns (potentially thousands per `cqs index --improve-docs --improve-all` run). When validation rejects a summary — e.g. "prompt-injection pattern detected" — the call surface emits an `Err`/`Vec<&str>` but no `tracing::warn!` is fired with the offending pattern name and chunk identifier. Investigating "why did this batch's summaries get dropped" requires re-running with debug logging that doesn't exist. Compare to e.g. `embed_query` which emits cache-aware completion events.
- **Suggested fix:** Inside `validate_summary`, when `ValidationOutcome::Rejected` is returned, emit `tracing::warn!(reason = ?outcome, text_len = text.len(), "Summary validation rejected")`. In `detect_all_injection_patterns`, emit `tracing::debug!(pattern = pat, "Injection pattern matched")` per match. No span needed; these are leaf calls.

#### `compute_rewrite` lacks span on the doc-writer parse-resolve-apply hot path
- **Difficulty:** easy
- **Location:** src/doc_writer/rewriter.rs:242
- **Description:** `pub fn compute_rewrite` reads the source file from disk, runs tree-sitter, and applies edits. It is called by both `rewrite_file` (which has its own `info_span!` at line 290) and `write_proposed_patch` (line 530, also has a span). But `compute_rewrite` is a `pub fn` documented as the "pure parse-resolve-apply step shared by" both wrappers. External callers (or future callers) that hit this entry point directly bypass any span. The function does non-trivial work — file read + tree-sitter parse + N edit applications — and a span here would let operators see resolve failures (the silent "function not found in re-parse" branch noted in the doc-comment) at debug.
- **Suggested fix:** Add `let _span = tracing::info_span!("compute_rewrite", path = %path.display(), edit_count = edits.len()).entered();` at the top of `compute_rewrite`. The two wrapper spans become parent contexts when the wrappers are entered first; direct callers get coverage they currently lack.

#### `config.rs:582` emits redundant `eprintln!` next to a `tracing::warn!` for the same event
- **Difficulty:** easy
- **Location:** src/config.rs:582-594
- **Description:** When too many references are configured, the validator emits both an `eprintln!("Warning: ...")` AND a `tracing::warn!(count, max, "Too many references configured, truncating")` for the same event. The duplication is intentional ("warn the operator on the terminal") but it routes through two channels — the `eprintln!` lands in journald as unstructured stderr (since the daemon has no terminal), while the `tracing::warn!` lands as structured JSON. Operators get the same event twice with different shapes. The contract elsewhere in the codebase (see OB-V1.30.1-9 in `cli/watch/events.rs:190`) is "tracing only; the daemon has no TTY". This file pre-dates that rule.
- **Suggested fix:** Drop the `eprintln!` block at lines 582-588. The `tracing::warn!` already covers the operator-visibility need — `cqs watch` prints it via the configured subscriber, CLI runs see it because the default subscriber writes warns to stderr. Keep only `tracing::warn!`.

#### `find_ld_library_dir` failure mode is silent — no log when no LD_LIBRARY_PATH dir is selected
- **Difficulty:** easy
- **Location:** src/embedder/provider.rs:121
- **Description:** On Linux startup, `find_ld_library_dir` parses `LD_LIBRARY_PATH`, looks for a directory that ORT's symlink-providers logic should target, and returns `None` if none qualifies. The `None` path is hit when (a) `LD_LIBRARY_PATH` is unset (legitimate), or (b) every entry is empty/non-existent/the ORT cache itself (a misconfiguration). Both collapse to silent `None`. When CUDA provider load mysteriously fails downstream and the user reports "no GPU detected", there's no breadcrumb showing whether the LD-resolve step ran and what it saw.
- **Suggested fix:** Emit `tracing::debug!(ld_path = %ld_path, ort_lib_dir = %ort_lib_dir.display(), "Selected LD_LIBRARY_PATH dir for provider symlinks")` on the Some branch and `tracing::debug!(ld_path_set = !ld_path.is_empty(), entries = ld_path.matches(':').count() + 1, "No qualifying LD_LIBRARY_PATH entry for provider symlinks")` on the None branch.

#### `Embedder::token_count` and `token_counts_batch` lack spans on the tokenizer hot path
- **Difficulty:** easy
- **Location:** src/embedder/mod.rs:715, src/embedder/mod.rs:737
- **Description:** Both `pub fn` lack `info_span!`. They are called per-chunk during indexing and per-query during retrieval (token-count gating drives splits/window selection — see `split_into_windows`). Surrounding methods like `embed_documents`, `embed_query`, `warm`, `split_into_windows` all carry spans. Token counting is non-trivial work on a long input (full BPE/wordpiece tokenization), and when an indexing run is slow on large files, the operator currently can't see whether time is in token_count vs the actual ONNX forward pass.
- **Suggested fix:** Add `let _span = tracing::debug_span!("token_count", text_len = text.len()).entered();` to `token_count` and `let _span = tracing::debug_span!("token_counts_batch", count = texts.len()).entered();` to `token_counts_batch`. Debug-level (not info) because per-chunk in tight loops at info would flood journalctl.

#### `cagra_persist_enabled` logs once but skips chunk_count / dim context
- **Difficulty:** easy
- **Location:** src/cagra.rs:859-868
- **Description:** When `CQS_CAGRA_PERSIST=0` is set, the function emits `tracing::info!("CQS_CAGRA_PERSIST=0 — CAGRA persistence disabled")` exactly once (OnceLock). The log carries no fields. When operators set this var to debug a CAGRA load failure, they have no breadcrumb tying the disable-decision to which slot/index was loading at the time. Several call sites (`cagra_save`, `cagra_load`, `cagra_save_with_store`) all return early on `!cagra_persist_enabled()` without their own warn — silent skip of CAGRA persistence is exactly the kind of "looked done, did nothing" failure mode CLAUDE.md memory flags.
- **Suggested fix:** Inside each `if !cagra_persist_enabled()` early-return arm at lines 890, 985, 1091, emit `tracing::warn!(path = %path.display(), "CAGRA op skipped — CQS_CAGRA_PERSIST=0")`. The OnceLock startup line is fine; the per-call warn is what a debugging operator actually needs.

#### `splade::probe_model_vocab` has a span but no completion event with elapsed_ms / vocab_size
- **Difficulty:** easy
- **Location:** src/splade/mod.rs:127
- **Description:** `probe_model_vocab` enters a `debug_span!("probe_model_vocab", path = ...)` but emits no completion event tying together how long the probe took and what vocab_size it read. The probe involves loading an ONNX model header — non-trivial IO + parse — and is called once per encoder construction. When SPLADE startup is slow, operators currently can't tell whether the probe was the bottleneck. Compare to `embed_documents` (line 877) which emits a completion `tracing::info!` with `elapsed_ms`, `total`, `dim`.
- **Suggested fix:** Capture `let started = std::time::Instant::now();` at the span entry and emit `tracing::debug!(vocab_size = ?probed, elapsed_ms = started.elapsed().as_millis() as u64, "probe_model_vocab complete")` on the Ok return path.

#### `worktree.rs::record_worktree_stale` and `is_worktree_stale` lack tracing on staleness state changes
- **Difficulty:** easy
- **Location:** src/worktree.rs:219, src/worktree.rs:229
- **Description:** Both functions touch the cross-worktree staleness flag (a marker file or atomic) but emit no tracing. `record_worktree_stale` is the producer side ("our worktree's index is now stale"); `is_worktree_stale` is the consumer side ("should I show a warning to the user"). When a multi-worktree workflow misbehaves — agent A's reindex doesn't propagate the stale flag, agent B keeps serving stale results — there's no journal trail to diagnose which side dropped the signal. Note CLAUDE.md memory specifically calls out worktree leakage (#1254) as a pain point; observability here is exactly what would make that diagnosable.
- **Suggested fix:** Add `tracing::info!(worktree_root = %worktree_root.display(), "worktree marked stale")` at the producer (`record_worktree_stale`) and `tracing::debug!(stale = res, "is_worktree_stale check")` at the consumer.

DONE
