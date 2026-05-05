## Performance

#### [PF-V1.30-1]: `reindex_files` watch path double-parses calls per file (parse_file_all then extract_calls_from_chunk per chunk)
- **Difficulty:** medium
- **Location:** `src/cli/watch.rs:2815, 2930-2939`
- **Description:** The watch reindex calls `parser.parse_file_all(&abs_path)` at line 2815 — this returns `(file_chunks, calls, chunk_type_refs)`, where `calls` is the file-level call graph. The `calls` value is upserted at line 2851 via `store.upsert_function_calls`, then **silently discarded for chunk-level call mapping**. Lines 2930-2939 then loop every chunk and call `parser.extract_calls_from_chunk(chunk)` — which re-runs tree-sitter over the chunk content to extract the same call sites a second time. The bulk pipeline already fixed this in P2 #63 by using `parse_file_all_with_chunk_calls` (returns a fourth `chunk_calls: Vec<(chunk_id, CallSite)>` value from the same Pass 2). The docstring at `src/parser/mod.rs:447-451` explicitly notes "Watch (`src/cli/watch.rs`) still uses `parse_file_all` and runs its own `extract_calls_from_chunk` per chunk; collapsing that into this method is a separate refactor." That refactor never landed. With ~14k chunks per repo-wide reindex (parser.rs note) and one tree-sitter parse per chunk, this is an extra 14k tree-sitter parses per `cqs index` (when the daemon is the indexer) or per touched file's chunks per watch event.
- **Suggested fix:** Switch the watch path from `parse_file_all` to `parse_file_all_with_chunk_calls`. The fourth tuple element is `Vec<(String, CallSite)>` keyed by absolute-path chunk id; rewrite the ids using the same prefix-strip the watch path already does for `chunk.id` at line 2834, then replace the `for chunk in &chunks { extract_calls_from_chunk(chunk) }` loop with a `HashMap` populated from the returned chunk_calls. Single-line API switch + ~10 lines of id rewriting; cuts reindex CPU roughly in half on the watch path.

#### [PF-V1.30-2]: `reindex_files` watch path bypasses the global EmbeddingCache (slot/cross-slot benefit lost on file edits)
- **Difficulty:** medium
- **Location:** `src/cli/watch.rs:2876-2887` vs `src/cli/pipeline/embedding.rs:39-62`
- **Description:** PR #1105 added the per-project `.cqs/embeddings_cache.db` keyed by `(content_hash, model_id)` so a chunk re-embedded after a model swap or a new slot can hit cache instead of going through the GPU. The bulk index path (`prepare_for_embedding`) checks both `global_cache.read_batch` and `store.get_embeddings_by_hashes`. The watch reindex hot path (`reindex_files`) at line 2877 only calls `store.get_embeddings_by_hashes(&hashes)` — it never sees `EmbeddingCache`. Net effect: every file change in watch mode goes through the embedder for any chunk whose content_hash isn't in the *current slot's* `chunks.embedding` column, even if the same hash was already computed in another slot or in a prior model that lives in the global cache. The watch loop is the highest-frequency embedder consumer (every file save during active development); missing the global cache here costs the most.
- **Suggested fix:** Plumb `global_cache: Option<&EmbeddingCache>` through `cmd_watch` → `reindex_files`. Replace lines 2876-2887 with a call to the same `prepare_for_embedding` helper the bulk pipeline uses (it already handles the `global cache → store cache → embed` fallback chain, including the dim mismatch guard). Eliminates the diverging cache-check code and makes the watch path benefit from #1105 the way the bulk path already does.

#### [PF-V1.30-3]: `reindex_files` allocates N empty `Embedding` placeholders then overwrites each
- **Difficulty:** easy
- **Location:** `src/cli/watch.rs:2918-2924`
- **Description:** `let mut embeddings: Vec<Embedding> = vec![Embedding::new(vec![]); chunk_count];` allocates `chunk_count` placeholder `Embedding` structs (each with an empty inner `Vec<f32>`), then immediately overwrites every slot via the cached + new-embedding loops at 2919-2923. Even setting aside the constructor cost, the `Embedding` type holds normalized-state metadata; the placeholders may need `Embedding::try_new(vec![])` validation in a future refactor and silently produce zero-norm vectors today. Allocation pattern is also wasteful — for a 100-file batch with 3000 chunks, that's 3000 `Embedding::new(vec![])` calls with discarded results.
- **Suggested fix:** Build `embeddings` directly from the (cached, new) iterators rather than placeholder-then-overwrite. Either: (1) sort `cached` and `to_embed` indices and merge in order, or (2) build a `HashMap<usize, Embedding>` and `(0..chunk_count).map(|i| map.remove(&i).unwrap_or_else(...))` — but better is to refactor the same way the bulk pipeline does (`create_embedded_batch` at `src/cli/pipeline/embedding.rs:127-143`): zip cached + (to_embed/new_embeddings) in original order without ever materializing a placeholder Vec. This is the same pattern the bulk path already proved.

#### [PF-V1.30-4]: `prepare_for_embedding` always issues store-cache query even when global cache fully satisfies the batch
- **Difficulty:** easy
- **Location:** `src/cli/pipeline/embedding.rs:64-82`
- **Description:** `prepare_for_embedding` first queries the global `EmbeddingCache` (line 47) populating `global_hits`, then UNCONDITIONALLY queries the store cache (line 68) for the same `hashes` slice. On the warm-cache path (e.g. reindex after `cqs slot promote`, or any reindex where chunks are unchanged), the global cache hit-rate approaches 100% and every store query is wasted DB work. The store query at `get_embeddings_by_hashes` is one SELECT but with O(n) bind variables and a JOIN against the `chunks` table — non-trivial latency on big batches. The fix is to filter the second query to only hashes the global cache missed.
- **Suggested fix:** Compute `let missed_hashes: Vec<&str> = hashes.iter().filter(|h| !global_hits.contains_key(*h)).copied().collect()` and pass `&missed_hashes` to `store.get_embeddings_by_hashes`. When all chunks hit global cache, the store query is skipped entirely. When none do, behaviour is identical to today. Additional comment at line 84 about the `global cache > store cache > embed` precedence is already correct; the implementation just doesn't act on it for the second query.

#### [PF-V1.30-5]: `wrap_value` deep-clones the entire payload via `serde_json::to_value(Envelope::ok(&payload))`
- **Difficulty:** medium
- **Location:** `src/cli/json_envelope.rs:160-176`
- **Description:** `wrap_value(&serde_json::Value)` constructs `Envelope::ok(payload)` (which holds `&Value`), then serializes-and-parses the whole envelope via `serde_json::to_value`. For `serde_json::Value` the `Serialize` impl visits every node and rebuilds an identical tree — a deep clone disguised as a re-serialization round trip. The function is called once per daemon dispatch via `crate::cli::batch::write_json_line` and once per CLI emit, so every `cqs gather --tokens 50000` (which can be 50KB+ of nested objects), every `cqs scout`, every `cqs review` output pays the cost. The header comment at line 153-155 acknowledges "shallow clone of the payload (necessary because `serde_json::json!` macro takes ownership)" — but this isn't shallow, the serde_json round trip walks the whole tree and reallocates every Map and Vec. For a typical 30KB gather payload, that's ~30KB of allocator churn per query; on a busy daemon at 100 QPS that's ~3MB/s of pointless allocator pressure plus the CPU walking the tree.
- **Suggested fix:** Build the envelope as a `serde_json::Value::Object` directly without a typed-struct round trip. `serde_json::Map::from_iter([("data", payload.clone()), ("error", Value::Null), ("version", Value::Number(1.into()))])`. Single shallow clone of the payload's outer enum tag (the inner Map/Vec stays owned) instead of a tree walk. Even better: change the contract so callers pass an *owned* `serde_json::Value` and `wrap_value` moves it in — `Map::insert("data", payload)` doesn't allocate a copy at all. Most call sites (`batch/mod.rs::write_json_line`) already produce the value just-in-time; switching to by-value is a per-site noop.

#### [PF-V1.30-6]: Daemon socket handler walks the args array twice (validation pass + extraction pass)
- **Difficulty:** easy
- **Location:** `src/cli/watch.rs:266-297`
- **Description:** `handle_socket_client` first scans `request.get("args")` to collect indices of non-string elements (`bad_arg_indices`, lines 267-274), and if the array is clean does a SECOND pass via `arr.iter().filter_map(|v| v.as_str().map(String::from))` (lines 291-296) to materialize the `Vec<String>`. Each daemon query thus walks the `serde_json::Value::Array` twice. Cheap individually but it's literally the request entry point — every daemon query at 100+ QPS pays this. Combine the two passes: do the strict-string validation while building the `Vec<String>` and bail out the moment a non-string is observed.
- **Suggested fix:** Fold both passes into one:
```rust
let mut args = Vec::new();
let mut bad_arg_indices = Vec::new();
if let Some(arr) = request.get("args").and_then(|v| v.as_array()) {
    for (i, v) in arr.iter().enumerate() {
        match v.as_str() {
            Some(s) => args.push(s.to_string()),
            None => bad_arg_indices.push(i),
        }
    }
}
if !bad_arg_indices.is_empty() { /* reject */ }
```
One pass instead of two; preserves the existing reject-with-indices error message.

#### [PF-V1.30-7]: `build_graph` correlated subquery for n_callers — N rows × per-row COUNT(*) instead of one GROUP BY
- **Difficulty:** medium
- **Location:** `src/serve/data.rs:234-264`
- **Description:** The node-fetch SQL in `build_graph` includes `COALESCE((SELECT COUNT(*) FROM function_calls fc WHERE fc.callee_name = c.name), 0) AS n_callers_global` as a correlated subquery in the SELECT. SQLite executes the subquery once per row scanned. With `idx_callee_name` present the per-row cost is O(log M) where M = function_calls row count (~30k+ in this repo), and N is the cap'd graph size (`ABS_MAX_GRAPH_NODES`, currently 5000). That's 5000 × log(30k) ≈ 75k index probes for one `/api/graph` request. A single `LEFT JOIN (SELECT callee_name, COUNT(*) AS n FROM function_calls GROUP BY callee_name)` aggregates once and joins by name — one full scan + one hash join, O(M + N), independent of N. On larger projects (the cqs serve /api/graph endpoint is the biggest data fetch in the new web surface) the difference is several hundred ms vs single-digit ms.
- **Suggested fix:** Replace the correlated subquery with a JOIN against an aggregated subselect:
```sql
SELECT c.id, c.name, c.chunk_type, c.language, c.origin, c.line_start, c.line_end,
       COALESCE(cc.n, 0) AS n_callers_global
FROM chunks c
LEFT JOIN (SELECT callee_name, COUNT(*) AS n FROM function_calls GROUP BY callee_name) cc
  ON cc.callee_name = c.name
WHERE 1=1 ... ORDER BY n_callers_global DESC, c.id ASC LIMIT ?
```
Same result, single aggregation pass. Also benefits `build_hierarchy` which has a similar shape (`src/serve/data.rs:670-754`).

#### [PF-V1.30-8]: `build_graph` edge-dedup HashSet keys clone (file, caller, callee) per row even on dedup miss
- **Difficulty:** easy
- **Location:** `src/serve/data.rs:367-373`
- **Description:** The edge dedup loop builds `let key = (file.clone(), caller.clone(), callee.clone())` for every row regardless of whether the row will be kept, then `seen.insert(key)` — three String clones per fetched row. With `ABS_MAX_GRAPH_EDGES` typical at tens of thousands, that's tens of thousands of extra String allocations per `/api/graph` request, most of them duplicating work the row decode already did (`row.get("file")` already returned an owned String). The pattern was lifted from a deduplicating insert in another module but here the strings are small and the surrounding loop bound is ABS_MAX_GRAPH_EDGES so the cost compounds.
- **Suggested fix:** Two options. (1) Skip the dedup entirely — the SQL `LIMIT` + the symmetric `IN (...)` twice over already over-fetches; deduping at the resolver step at line 396 is enough since the resolver is a `HashMap` lookup that naturally collapses duplicates by ignoring them. (2) Keep the dedup but switch to a hash-of-bytes key:
```rust
use std::collections::hash_map::DefaultHasher;
let mut h = DefaultHasher::new();
file.hash(&mut h); caller_name.hash(&mut h); callee_name.hash(&mut h);
let hash_key = h.finish();
if seen.insert(hash_key) { accum.push((file, caller_name, callee_name)); }
```
Hash collisions on a `u64` keyed `HashSet<u64>` are negligible at <1M edges. Cuts allocations from 3N+1 strings to ~zero.

#### [PF-V1.30-9]: `extract_imports` uses `HashSet<String>` — allocates a `String` per candidate line even on duplicate rejection
- **Difficulty:** easy
- **Location:** `src/where_to_add.rs:258-276`
- **Description:** `extract_imports` iterates every line of every chunk, and for every line that matches a prefix it calls `seen.insert(trimmed.to_string())`. The HashSet stores `String` so insertion always allocates, even when the value is rejected as a duplicate (HashSet still hashes its borrowed key, but the caller materialized the String first). For a Rust file with ~50 chunks × ~30 lines/chunk × 5 prefixes, that's ~7500 `to_string` calls per `cqs where`/`cqs task` invocation — most of which are non-import lines that matched the prefix loosely or duplicate imports already seen. Lines borrowed from `chunks` are valid for the lifetime of the function so a borrowed-key HashSet works.
- **Suggested fix:** Switch `seen` to `HashSet<&str>` with the same lifetime as `chunks`:
```rust
let mut seen: HashSet<&str> = HashSet::new();
let mut imports: Vec<String> = Vec::new();
for chunk in chunks {
    for line in chunk.content.lines() {
        let trimmed = line.trim();
        for &prefix in prefixes {
            if trimmed.starts_with(prefix) && imports.len() < max && seen.insert(trimmed) {
                imports.push(trimmed.to_string());  // Allocate only on accept
                break;
            }
        }
    }
}
```
Allocation now happens only for accepted imports (capped at `max=5`), not per candidate line. ~1500× fewer String allocations on a typical Rust file.

#### [PF-V1.30-10]: Watch `reindex_files` cached embedding clone via `existing.get` instead of `.remove`
- **Difficulty:** easy
- **Location:** `src/cli/watch.rs:2879-2887`
- **Description:** The cached-embedding loop:
```rust
for (i, chunk) in chunks.iter().enumerate() {
    if let Some(emb) = existing.get(&chunk.content_hash) {
        cached.push((i, emb.clone()));   // clone every cached Embedding
    } else {
        to_embed.push((i, chunk));
    }
}
```
Every cache hit clones the `Embedding` (inner `Vec<f32>`, dim=1024 default = 4KB allocation per hit). For a 100-file save that touches 500 chunks with 80% cache hit rate, that's ~400 × 4KB = 1.6MB of allocator churn per watch event — and watch events fire on every save in active development. The `existing` map is consumed only by this loop and discarded afterward, so we can `.remove()` to take ownership instead.
- **Suggested fix:** Make `existing` mutable (already is — `let mut`isn't there but the binding owns the map) and use `existing.remove(&chunk.content_hash)` to take ownership:
```rust
let mut existing = store.get_embeddings_by_hashes(&hashes)?;
let mut cached: Vec<(usize, Embedding)> = Vec::new();
let mut to_embed: Vec<(usize, &cqs::Chunk)> = Vec::new();
for (i, chunk) in chunks.iter().enumerate() {
    if let Some(emb) = existing.remove(&chunk.content_hash) {
        cached.push((i, emb));
    } else {
        to_embed.push((i, chunk));
    }
}
```
Eliminates every Embedding clone on the cache-hit path. Mirrors the `global_hits.remove` pattern already used at `src/cli/pipeline/embedding.rs:97`. P3 #126-style fix the watch path missed.
