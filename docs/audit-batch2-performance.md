# Performance Audit (post-v1.38.0)

Scope: hot-path allocations, N+1 patterns, missing caches, env-var thrash, redundant I/O. Focus on CLI/daemon search and indexing critical paths. Audit mode ON (no notes consulted).

Prior PF-V1.36-* findings already addressed; this batch covers gaps and post-v1.36 regressions.

---

#### PF-V1.38-1: `is_test_chunk` rebuilds 54-language pattern lists per call from inside the per-candidate scoring loop
- **Difficulty:** medium
- **Location:** `src/lib.rs:498-533` (`is_test_chunk`) → `src/language/mod.rs:977-1040` (`all_test_path_patterns`, `all_test_name_patterns`)
- **Description:** `apply_scoring_pipeline` (search/scoring/candidate.rs:368) calls `chunk_importance` → `is_test_chunk(name, file_path)` once per candidate. Each call iterates `language::REGISTRY.all_test_name_patterns()` and `all_test_path_patterns()`, both of which **rebuild a deduplicated `Vec<&'static str>` plus a `HashSet<&str>` from scratch by walking all 54 language definitions** (`for def in self.all() { for pat in def.test_path_patterns { seen.insert(*pat); ... } }`). Inside `is_test_chunk` itself: `let normalized = file.replace('\\', "/")` allocates a String even on Linux where no replacement happens, plus per-pattern `sql_like_matches` allocates another String (`pattern.replace("\\_", "_")`) and a `Vec<&str>` from `split('%').collect()`. For 500 candidates × ~60 path patterns + ~12 name patterns the per-query cost is roughly 30k–60k allocations. Hits every search at default `enable_demotion = true`. Bigger repos with more languages amplify — and `TEST_NAME_PATTERNS` rebuild has cost roughly proportional to language count.
- **Suggested fix:** Wrap both `all_test_*` calls in `OnceLock<Vec<&'static str>>` on `LanguageRegistry` (the registry itself is static — patterns can never change at runtime). Skip the `replace('\\', '/')` when `!file.contains('\\')` (cheap byte-scan). Hoist `sql_like_matches`'s `Vec<&str>` parts to a per-pattern `OnceLock`.

#### PF-V1.38-2: SPLADE `id_map: Vec<String>` not migrated to `Vec<Box<str>>`
- **Difficulty:** easy
- **Location:** `src/splade/index.rs:165, 578, 768`
- **Description:** PR #1502 migrated HNSW and CAGRA `id_map` from `Vec<String>` to `Vec<Box<str>>` (saves 8 bytes per entry: 24 → 16). SPLADE was not migrated despite identical access pattern (build-once, read-many; mutated only by `push` during `build`). For an 18k-chunk index at 32-char chunk ids, missing ~144 KB of heap savings per slot with a SPLADE backend. Stacks with HNSW/CAGRA savings on multi-slot setups.
- **Suggested fix:** Apply the #1502 pattern: change `id_map: Vec<String>` to `Vec<Box<str>>`, push `chunk_id.into_boxed_str()` in `build`, deserialize via `String::into_boxed_str()` in load path (line 578 area). The two read sites (`get(idx)` + `clone()` on line 257) work unchanged because `Box<str>` derefs to `&str`.

#### PF-V1.38-3: `resolve_splade_alpha` allocates `format!` and reads two env vars per search query
- **Difficulty:** easy
- **Location:** `src/search/router.rs:730-796`
- **Description:** Called once per search via `dispatch_search`. On every call: `format!("CQS_SPLADE_ALPHA_{}", category.to_string().to_uppercase())` allocates a String (line 743), then `std::env::var(&cat_key)` (744) and `std::env::var("CQS_SPLADE_ALPHA")` (781) syscall the env table. Comment on line 743 even acknowledges "hot path" yet no caching. The rest of the function then takes a `RwLock::read()` for the slot table. For batch-mode handlers that fire many searches per session, the env reads dominate the hot path post-fusion.
- **Suggested fix:** `OnceLock<HashMap<QueryCategory, Option<f32>>>` for the per-cat env-derived value (keyed by enum variant, so no string formatting). Same for the global `CQS_SPLADE_ALPHA` — `OnceLock<Option<f32>>`. The slot/preset/default fall-through stays as-is. Test `test_type_boost_factor_reads_env_on_each_call` indicates eval sweeps mutate env between searches in a single process — gate caching behind a "first read wins" model documented in the knob, or add a test-only reset hook (mirrors `reset_classifier_vocab_for_test`).

#### PF-V1.38-4: SPLADE `splade_max_chars()` and `default_threshold()` re-read env on every encode call
- **Difficulty:** easy
- **Location:** `src/splade/mod.rs:387-393` (`splade_max_chars`), `397+` (`default_threshold`), `815-819` (`CQS_SPLADE_MAX_SEQ` in `encode_batch`)
- **Description:** `encode()` is called per-chunk during indexing and per-query during search. For an 18k-chunk full reindex with SPLADE, that's 18k env-var lookups for `CQS_SPLADE_MAX_CHARS` plus 18k for `CQS_SPLADE_THRESHOLD`. `encode_batch` parses `CQS_SPLADE_MAX_SEQ` once per batch (less hot, but same shape). All three values are immutable for the process lifetime in production.
- **Suggested fix:** `OnceLock<usize>` / `OnceLock<f32>` initializers, mirroring the `PLACEHOLDER_CACHE`/`MULTISTEP_PATTERNS_AC` pattern already established. Document via `// cached at process start` comment so eval harnesses know not to mutate mid-run.

#### PF-V1.38-5: Tree-sitter `Parser::new()` allocated per `parse_file` call
- **Difficulty:** medium
- **Location:** `src/parser/mod.rs:295, 554, 914`; `src/parser/aspx.rs:234, 342, 478`; `src/parser/calls.rs:50, 314, 687`; `src/parser/injection.rs:275`; `src/parser/l5x.rs:76`
- **Description:** Every call to `parse_file` / `parse_file_all_with_chunk_calls` constructs a fresh `tree_sitter::Parser` and calls `set_language(&grammar)`. For a fresh index over 600+ files that's 600+ parser allocations + grammar reloads. Tree-sitter parsers are reusable across files of the same language — `set_language` is the only language-specific call. Recursion-depth state lives on each `parse()` invocation. The Parser struct already caches Queries lazily in `OnceCell` but parser instances aren't cached.
- **Suggested fix:** Add `parsers: HashMap<Language, Mutex<tree_sitter::Parser>>` to the Parser struct (alongside `queries`). Each `parse_file` does `let mut p = self.parsers.get(&lang).unwrap_or_init().lock(); p.set_language(...)?; let tree = p.parse(...);`. For multi-threaded indexing use `thread_local!` pools. Aspx/L5X subparsers benefit identically.

#### PF-V1.38-6: `gather::expand` clones `Arc<str>` neighbor to `String` for HashMap key on every BFS expansion
- **Difficulty:** easy
- **Location:** `src/gather.rs:362-364`
- **Description:** Inside the BFS expansion loop: `visited.insert(Arc::clone(&neighbor)); let key: String = neighbor.to_string(); name_scores.insert(key, ...);` — the neighbor is already an `Arc<str>` (cheap to clone), but `to_string()` materializes a fresh owned String. For a gather BFS that expands ~1000 nodes (default `max_expanded_nodes`) in a dense graph that's 1000 redundant String allocations.
- **Suggested fix:** Change `name_scores: HashMap<String, (f32, usize)>` to `HashMap<Arc<str>, (f32, usize)>`; use `Arc::clone(&neighbor)` (atomic incr, no alloc). The downstream `name_scores.get(name.as_ref())` (line 347) and `fetch_and_assemble` (line 397) consumers work unchanged via `Borrow<str>`.

#### PF-V1.38-7: `extract_call_snippet_from_cache` allocates full `Vec<&str>` of all chunk lines just to pick 3
- **Difficulty:** easy
- **Location:** `src/impact/analysis.rs:183-189`
- **Description:** `let lines: Vec<&str> = best.chunk.content.lines().collect();` allocates a Vec sized to the entire chunk's line count, then indexes `lines[start..end]` to extract a 3-line window. For chunks 100+ lines long and N callers per impact target (build_caller_info loop at line 140), this is `N × Vec::with_capacity(line_count)` of wasted allocation. The chunk content is already in memory; only the 3-line slice is needed.
- **Suggested fix:** Replace with `let snippet: Vec<&str> = best.chunk.content.lines().skip(start).take(end - start).collect(); Some(snippet.join("\n"))`. The iterator is lazy — only the 3 lines we keep get visited. Saves both the intermediate Vec and the work of fully iterating long chunks.

#### PF-V1.38-8: Watch reindex stats `abs_path` twice per file (mtime + size) — duplicate syscall
- **Difficulty:** easy
- **Location:** `src/cli/watch/reindex.rs:649-718`
- **Description:** First stat at line 659 (`abs_path.metadata().and_then(|m| m.modified())`) inside the `mtime_cache.entry(file.clone()).or_insert_with(...)` block. Second stat at line 718 (`std::fs::metadata(&abs_path).ok().map(|m| m.len())`) for the v23 fingerprint write-back. Both are on the same file in the same code block (line 717 already does `let abs_path = root.join(file);` again — second `PathBuf::join` allocation too). On WSL 9P the per-stat latency is ms-scale, so a 200-file watch tick burns 200ms on duplicate stats alone.
- **Suggested fix:** Combine into one `match abs_path.metadata() { Ok(m) => (m.modified(), Some(m.len())), Err(_) => (Err(_), None) }` so size + mtime come from one syscall. Hoist `let abs_path = root.join(file);` above the mtime block so the second `join` is gone too.

#### PF-V1.38-9: `analyze_type_impact` makes 4+ string clones per (chunk × type) edge in nested loop
- **Difficulty:** medium
- **Location:** `src/impact/analysis.rs:471-490`
- **Description:** For each `(type_name, chunks)` and each `chunk` in chunks: `shared.entry(chunk.name.clone()).or_default().insert(type_name.clone());` followed by `chunk_info.entry(chunk.name.clone()).or_insert((chunk.file.clone(), ...));`. That's 4 String/PathBuf clones per edge. For a popular type used by 200 functions × M types in scope, that's 800+×M clones — only 1 per unique name actually survives `.entry().or_default()`. The `chunk.name.clone()` happens twice per chunk regardless of whether either entry was new.
- **Suggested fix:** Use `shared.entry_ref(&chunk.name).or_insert_with(|| HashSet::new()).insert(...)` (hashbrown's `entry_ref` borrows the lookup key, allocates only on insert). Or pull `let name = &chunk.name;` and use `match shared.get_mut(name)` first, falling through to `shared.insert(name.clone(), ...)` on miss. Same shape for `chunk_info`. `type_name.clone()` is unavoidable for `HashSet<String>` insert but could be `Arc<str>` if inflation matters.

#### PF-V1.38-10: `find_test_chunks_cross` calls uncached `find_test_chunks()` once per project — N×LIKE-scans
- **Difficulty:** easy
- **Location:** `src/store/calls/cross_project.rs:217-237`
- **Description:** Loops `for ns in &self.stores { ns.store.find_test_chunks() }`. `find_test_chunks` is the per-store LIKE-scan PF-2 already flagged as uncached (`LIKE '%marker%'` over the BLOB content column). Cross-project users with N references pay N × full-table-scan per call. Sibling `merged_call_graph` (line 244) uses `ensure_all_graphs()` — a cache pattern — but `find_test_chunks_cross` doesn't.
- **Suggested fix:** Either (a) add a `RwLock<Option<Arc<Vec<TestChunkSummary>>>>` cache to each `Store` (mirrors `note_boost_cache`) so the per-project scan is amortized across the cross-project caller; or (b) in `CrossProjectStore`, cache the merged result at the cross-project level and invalidate only when an underlying store reports a write. (a) is the more general fix — `find_test_chunks` has 14 callers per PF-2.

---

## Summary

Ten findings, mostly easy, several with measurable per-query cost on the search hot path. Highest-leverage targets:

1. **PF-V1.38-1** (`is_test_chunk` registry rebuild) — fires per candidate per search; quick `OnceLock` patches knock 30k+ allocations off every query. Highest priority by far.
2. **PF-V1.38-3** + **PF-V1.38-4** (env var thrash on every search/encode) — small individually, compound on hot paths. SPLADE encode in particular hits 18k env reads per reindex.
3. **PF-V1.38-5** (tree-sitter Parser per file) — 600× per fresh index; the structural work to add a Mutex pool is real but the win is durable.
4. **PF-V1.38-2** (SPLADE id_map Box<str>) — exact analog of merged #1502; trivial.
5. **PF-V1.38-7** + **PF-V1.38-8** (snippet `lines().collect()` and duplicate stat) — easy hot-path wins on impact + watch reindex.

Findings 6, 9, 10 are medium-impact tidiness with solid call-path leverage. None of these have been triaged in audit-batch1-scaling.md or prior PF-V1.* files.

Worth noting: the search hot path (`search_filtered` → `search_by_candidate_ids_with_notes`) has been heavily optimized through PF-V1.25-* and PF-V1.36-* work — the surface left is mostly in upstream / downstream modules (router, scoring helpers, registry lookups) rather than the inner scoring loop itself.
