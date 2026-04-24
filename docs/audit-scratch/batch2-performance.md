## Performance

#### [PF-V1.29-1]: Daemon request path shell-joins and re-splits args on every query
- **Difficulty:** medium
- **Location:** `src/cli/watch.rs:315-331`
- **Description:** For every daemon socket query, `handle_socket_client` extracts `command: String` and `args: Vec<String>` from the JSON request (`src/cli/watch.rs:229-270`), then reconstructs a single string via `format!("{} {}", command, shell_words::join(&args))` and passes it to `BatchContext::dispatch_line`, which immediately re-splits it with `shell_words::split` (`src/cli/batch/mod.rs:563`). This is a pure waste on the hot daemon path — every query pays: (1) `shell_words::join` (quoting + escape pass, allocates per arg), (2) `format!` allocation of the assembled line, (3) `shell_words::split` on the daemon side (another allocation + tokenization pass), (4) both paths validate NUL bytes on the same data. For agents firing 100+ queries per task, this is hundreds of redundant String allocations and two full passes over the tokens. The Vec<String> that arrives already has the shape `BatchInput::try_parse_from(&tokens)` expects.
- **Suggested fix:** Add a `dispatch_tokens(&self, tokens: &[String], out: &mut impl Write)` method on `BatchContext` that takes already-parsed tokens directly. `handle_socket_client` prepends `command` to `args` (or uses `std::iter::once(command).chain(args.iter())`) and calls `dispatch_tokens`. `dispatch_line` can keep its shell parsing path for the `cqs batch` stdin surface but skips the round-trip for daemon queries. Also eliminates one of two `reject_null_tokens` checks since the JSON parser's string validation already covers NUL bytes on the socket path.

#### [PF-V1.29-2]: `fetch_chunks_by_ids_async` and `fetch_candidates_by_ids_async` hardcode BATCH_SIZE=500, ignore modern SQLite limit
- **Difficulty:** easy
- **Location:** `src/store/chunks/async_helpers.rs:27, 69` (both functions)
- **Description:** Both fetch helpers hardcode `const BATCH_SIZE: usize = 500` with comments claiming "SQLite's 999-parameter limit". That limit was raised to 32766 in SQLite 3.32 (2020). The rest of the codebase (`src/store/calls/query.rs:204`, `src/store/types.rs:18`, `src/store/sparse.rs:123`, `src/store/calls/crud.rs:32,81,115,217,283,313`) already uses `crate::store::helpers::sql::max_rows_per_statement(1)` which returns ~32466. These are called from every search: `search_by_candidate_ids_with_notes` → `fetch_candidates_by_ids_async` (line 860 of search/query.rs) and `finalize_results` → `fetch_chunks_by_ids_async` (line 412 of search/query.rs). On wide queries (e.g. `cqs search "X" --limit 100 --rerank` which pools to `limit * 3 = 300`), each call fits in one statement anyway — but the `cqs context` batch fetch (same helper) routinely hits 1000+ IDs and pays 2-3× the round trips.
- **Suggested fix:** Replace the two hardcoded `BATCH_SIZE = 500` with `max_rows_per_statement(1)`. Drop the stale "999-param limit" comment. Same one-line change the other modules already made.

#### [PF-V1.29-3]: `get_type_users_batch` and `get_types_used_by_batch` hardcode BATCH_SIZE=200 — impact analysis pays 3× latency
- **Difficulty:** easy
- **Location:** `src/store/types.rs:392, 438`
- **Description:** Both batch type-edge queries declare `const BATCH_SIZE: usize = 200`. On `cqs impact` for a function that uses 500+ types (common for Rust code — every `HashMap`, `Vec`, `Result`, custom struct counts), `find_type_impacted` at `src/impact/analysis.rs:450` drives 3+ SQL round trips per impact call when one would suffice. Each SQL JOIN (type_edges→chunks) also reloads the full chunk row. Adjacent batch functions in the same file switched to `max_rows_per_statement()` three versions ago; these two slipped through.
- **Suggested fix:** Replace both constants with `max_rows_per_statement(1)` (one bind per row). Already imported at `src/store/types.rs:18`. Single-line change per function.

#### [PF-V1.29-4]: `find_hotspots` allocates String for every callee in the graph before truncating
- **Difficulty:** easy
- **Location:** `src/impact/hints.rs:261-271`
- **Description:** `find_hotspots(graph, top_n)` iterates `graph.reverse.iter()`, calls `name.to_string()` on every entry to build a `Hotspot { name, caller_count }`, sorts the full Vec, then truncates to `top_n`. `graph.reverse` keys are `Arc<str>` (`src/store/calls/query.rs:113-117`). On a 15k-chunk codebase with ~5k distinct callees (reality per `cqs health --json`: 1838 for `assert`, 1771 for `assert_eq`, etc.), the function allocates 5k Strings for every call even though callers want `top_n = 5` (health) or `top_n = 20` (suggest). Pattern is O(n) allocations + O(n log n) sort when a bounded-heap + conditional allocation would be O(n log top_n) and `top_n` allocations.
- **Suggested fix:** Use a `BinaryHeap<(Reverse<usize>, Arc<str>)>` capped at `top_n`, pushing `(reverse.len(), Arc::clone(name))` (Arc clone is refcount bump, not alloc). Drain into `Vec<Hotspot>` at the end with exactly `top_n` `name.to_string()` calls. Cuts allocator churn on the health/suggest hot paths by ~250× for a 5k-callee graph with top_n=20.

#### [PF-V1.29-5]: Parser reads every source file, then unconditionally allocates a full CRLF-replaced copy
- **Difficulty:** easy
- **Location:** `src/parser/mod.rs:491`
- **Description:** `let source = source.replace("\r\n", "\n");` runs for every parsed file regardless of platform or actual CRLF presence. `String::replace` always allocates a fresh String the size of the input. On Linux (the primary development/CI platform) 99%+ of files have no CRLF, yet every parse pays a full-content allocation + memcpy. For the cqs codebase that's 607 files ranging up to 100KB+; on a fresh `cqs index` that's ~50MB of wasted allocations plus the I/O pressure from zeroing the new buffers. `source.contains("\r\n")` is a single linear scan with no allocation — cheap to check before allocating.
- **Suggested fix:** Guard the replace: `let source = if source.contains("\r\n") { source.replace("\r\n", "\n") } else { source };` Preserves CRLF-normalization semantics for actual CRLF files (Windows-authored docs, some config formats) while eliminating the alloc on the common case. Alternatively, use `memchr`-based scan for the `\r` byte only.

#### [PF-V1.29-6]: `BatchContext::notes()` clones the full notes Vec on every cache hit
- **Difficulty:** medium
- **Location:** `src/cli/batch/mod.rs:1015-1064`
- **Description:** `notes()` returns `Vec<cqs::note::Note>` and unconditionally clones the cached Vec on every call (`cached.as_ref()?.clone()` at line 1021 and `result = notes.clone()` at line 1061). For 202 notes (per `cqs health` in this repo), each call clones 202 `Note` structs — each carries `text: String`, `mentions: Vec<String>`, and other owned fields. Callers at `src/cli/batch/handlers/misc.rs:92` (scout), `src/cli/batch/handlers/info.rs:365, 400` (notes list, warnings) only need read access. Compare to sibling `test_chunks()` (line 1101) and `call_graph()` (line 1083) which correctly return `Arc<...>` for cheap O(1) clone. The inline comment at line 1004-1006 about cheap `AuditMode` cloning is correct for audit state but `notes()` is pasted-in and structurally different.
- **Suggested fix:** Change the cache type from `RefCell<Option<Vec<Note>>>` to `RefCell<Option<Arc<Vec<Note>>>>`. Return `Arc<Vec<Note>>`. Update three call sites (`misc.rs:92`, `info.rs:365, 400`) to match — they currently `&notes` and iterate, trivial change. Saves 202 String allocations × 3 call sites per batch query that touches notes.

#### [PF-V1.29-7]: `notes.rs::upsert_notes_batch` runs 3 SQL statements per note in a loop
- **Difficulty:** medium
- **Location:** `src/store/notes.rs:76-87` and the inner `insert_note_with_fts` at `src/store/notes.rs:30-58`
- **Description:** `upsert_notes_batch` loops over notes, calling `insert_note_with_fts` for each. That helper runs 3 statements: INSERT OR REPLACE into `notes` + DELETE from `notes_fts` + INSERT into `notes_fts`. For 200 notes, that's 600 prepared-statement round trips within the transaction. Unlike `upsert_chunks_batch` (which batches into multi-row INSERTs at `src/store/chunks/crud.rs:214`), notes use the per-row path. `replace_notes_for_file` at line 124-128 has the same pattern. Notes are smaller than chunks but the watch loop reindexes notes on every notes.toml edit — with 200+ notes and active note editing during audit sessions, this is ~3000× the round-trip overhead of a batched insert.
- **Suggested fix:** Follow the `upsert_chunks_batch` pattern — build a `QueryBuilder` that emits `INSERT OR REPLACE INTO notes (...) VALUES (?,?,?), (?,?,?), ...` chunked at `max_rows_per_statement(N)` rows per statement. FTS5 unfortunately doesn't support multi-row INSERT via `QueryBuilder::push_values` as cleanly (FTS5 has virtual-table quirks), but batching the DELETE `WHERE id IN (?,?,?...)` collapses N DELETEs into one, leaving only the per-row INSERT INTO notes_fts.

#### [PF-V1.29-8]: `prune_missing` fires `dunce::canonicalize` syscall per missing-path candidate
- **Difficulty:** medium
- **Location:** `src/store/chunks/staleness.rs:27-47` (`origin_exists`) called from `src/store/chunks/staleness.rs:88`
- **Description:** `prune_missing` enumerates all distinct file origins in the chunks table (often 10k+ on real-world projects), then for each one calls `origin_exists(origin, existing_files, root)`. That function first does a HashSet lookup; on miss it falls through to `dunce::canonicalize(&absolute)`, which is a real filesystem syscall per candidate. On the watch hot path with incremental reindex, this fires every reindex cycle; on the initial `cqs index` it fires for every origin in the DB. If `existing_files` was built with canonicalized paths and chunk origins are stored relative (the common case), *every* origin takes the canonicalize fallback. For 15k chunks and 607 distinct origins (per cqs health) that's 607 extra syscalls per prune. WSL filesystem canonicalize over NTFS mount is notoriously slow (~100µs per call) so this can be 60ms per prune on top of the actual delete cost.
- **Suggested fix:** Either: (1) normalize `existing_files` to also contain the relative form at build time so the cheap HashSet path always hits; or (2) build a second HashSet of origins that appear in chunks and subtract from `existing_files` via set difference (O(n+m) instead of O(n×syscall)). Or (3) canonicalize origins once at index time and store the canonical form so staleness is a pure HashSet lookup. Option 3 is the cleanest but requires schema touch; option 1 is zero-schema and resolves the WSL hot spot.

#### [PF-V1.29-9]: `suggest_tests` calls `reverse_bfs` inside a loop over callers — O(callers × graph_size)
- **Difficulty:** hard
- **Location:** `src/impact/analysis.rs:320-335`
- **Description:** For every caller in `impact.callers`, `suggest_tests` runs a fresh `reverse_bfs(&graph, &caller.name, DEFAULT_MAX_TEST_SEARCH_DEPTH)` to determine if that caller is reached by any test. The inline comment at line 322-327 acknowledges the concern but justifies it as "caller count is typically small". On a function with 50+ direct callers (typical for utility functions in a 15k-chunk codebase — `find_hotspots` output shows some functions with 1800+ callers), this is 50 graph traversals, each potentially visiting thousands of ancestor nodes up to depth 5. Degrades with codebase size and test-graph connectivity. The comment claims `reverse_bfs_multi_attributed` can't replace it because it attributes to only one source, but a single forward `bfs_from_tests` (starting at test nodes, walking to targets up to MAX_TEST_SEARCH_DEPTH) computes "is X reached by any test?" for every X in one pass.
- **Suggested fix:** Replace the per-caller BFS with a single pre-computed `reachable_from_tests: HashSet<&str>` — do one forward BFS from each test chunk up to depth N, union the reached sets. Then `is_tested = reachable_from_tests.contains(&caller.name)` is O(1). Reuses the same `graph.forward` adjacency. Cuts `cqs impact --suggest-tests` latency from O(callers × graph) to O(tests + callers). Even on small codebases the computation amortizes; for the cqs self-check with 3531 test chunks, the savings are substantial.

#### [PF-V1.29-10]: `search/query.rs` finalize_results clones sanitized FTS string for no reason
- **Difficulty:** easy
- **Location:** `src/search/query.rs:363-369`
- **Description:** In `finalize_results`:
```rust
let sanitized = sanitize_fts_query(&normalized);
let expanded = expand_query_for_fts(&sanitized);
let fts_query = if expanded.is_empty() {
    sanitized.clone()    // <-- unnecessary clone
} else {
    expanded
};
```
`sanitized` is owned and not referenced after line 366. The `.clone()` allocates a fresh String copy on every RRF search. A plain move works here — `sanitized` would be dropped on the `else` branch anyway since `expanded` is taken. Runs on every RRF-enabled search (the default path). A typical query string is ~30-100 bytes; over 1000 queries that's ~100KB of allocator churn, but more importantly it's a zero-cost fix.
- **Suggested fix:** `let fts_query = if expanded.is_empty() { sanitized } else { expanded };` Drop `.clone()`. The surrounding block owns `sanitized`; no borrow escapes.
