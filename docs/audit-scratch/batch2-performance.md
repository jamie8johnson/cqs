## Performance

#### PERF-V1.36-1: `fetch_candidates_by_ids_async` rebuilds id-position map per call instead of hashing on placeholder bind
- **Difficulty:** easy
- **Location:** src/store/chunks/async_helpers.rs:86-90
- **Description:** Every search hits this hot path. For ~200-500 candidate ids it allocates a `HashMap<&str, usize>` plus a `Vec<Option<(CandidateRow, Vec<u8>)>>` of `ids.len()` slots, then iterates rows to drop them into slots. The current code is fine *complexity*-wise (linear), but the slot vector wastes one `Option<>` discriminant per id (~32 bytes × N) and every miss leaves a `None` that gets walked again in `flatten()`. With ~32k candidates this is ~1MB of `Option` discriminants pushed across cache lines, then walked twice. A flat `Vec::with_capacity(ids.len())` plus a single re-sort by `id_pos.get(&candidate.id)` is cheaper and a smaller working set; or pre-allocate `Vec<MaybeUninit<...>>` with a presence bitmap. Comment claims this is the "fast" path but `Vec<Option<T>>` reorder isn't free and the `flatten()` collect loses the original allocation anyway.
- **Suggested fix:** Build `Vec<(CandidateRow, Vec<u8>)>` with `with_capacity(ids.len())`, then `sort_unstable_by_key(|(c, _)| id_pos[&c.id])` once at the end. One alloc, no `Option` discriminant churn, deterministic.

#### PERF-V1.36-2: `lang_set` / `type_set` rebuild from enum-to-string-to-lowercase on every search call
- **Difficulty:** easy
- **Location:** src/search/query.rs:849-856
- **Description:** Per search, `filter.languages` and `filter.include_types` are converted to `HashSet<String>` via `iter().map(|l| l.to_string().to_lowercase()).collect()`. Both `Language` and `ChunkType` are enums whose `Display` yields canonical lowercase already (the comment at line 861-865 explicitly says "DB values are already canonical lowercase from `Language::to_string` / `ChunkType::to_string`"). The `to_lowercase()` allocates a fresh `String` per variant for no reason, and rebuilding the set per search is wasteful — enums have `&'static str` representations the set could store as `&'static str`. A 50-search session with 10-language filters does 500 needless heap allocations.
- **Suggested fix:** Make `Language::as_str` and `ChunkType::as_str` return `&'static str`, then build `HashSet<&'static str>` from `filter.languages`/`filter.include_types` (no allocation). Compare via `lang_set.contains(candidate.language.as_str())`. Drops the `.to_lowercase()` and `String` allocs entirely.

#### PERF-V1.36-3: `ChunkRow::from_row` does ~16 column-name string lookups per row
- **Difficulty:** medium
- **Location:** src/store/helpers/rows.rs:72-100, also `from_row_lightweight` at 107-130
- **Description:** Every column read (`row.get("id")`, `row.get("origin")`, ...) does a linear scan of `SqliteRow::column_index_by_name` against the column-name list. For a 16-column SELECT with N rows this is `16N` strcmps. Search hydrates 100-500 rows per query; over 50 searches that's 80k-400k strcmps purely for column-name resolution. Indexed access via `row.get(0)`, `row.get(1)` etc. would skip the lookup entirely; the SELECT order is fixed in the same module (async_helpers.rs:34-36 and 92-96).
- **Suggested fix:** Switch to ordinal `row.get::<_, _>(0)`, `row.get(1)` etc. in `ChunkRow::from_row` and `CandidateRow::from_row`. Keep the SELECT column order pinned in a const string constant adjacent to the `from_row` so the contract is local.

#### PERF-V1.36-4: `fetch_and_assemble` does double hashmap lookup + clone per gathered chunk
- **Difficulty:** easy
- **Location:** src/gather.rs:417-420
- **Description:** Anti-pattern `if seen_ids.contains(&r.chunk.id) { continue; } seen_ids.insert(r.chunk.id.clone());` does two hash probes and clones the id String regardless of outcome. Same anti-pattern at src/llm/doc_comments.rs:333-337 and src/cli/commands/graph/explain.rs:251 / 442. `HashSet::insert` returns `bool` already.
- **Suggested fix:** `if !seen_ids.insert(r.chunk.id.clone()) { continue; }` — single probe, clone happens only when actually inserting. Even better: `if !seen_ids.contains(r.chunk.id.as_str()) { seen_ids.insert(r.chunk.id.clone()); chunks.push(...); }` if `seen_ids` were `HashSet<String>` keyed by `&str` borrow.

#### PERF-V1.36-5: `extract_imports_regex` allocates `String` per probed line even when already in seen set
- **Difficulty:** easy
- **Location:** src/where_to_add.rs:807-820
- **Description:** Inner loop calls `seen.insert(trimmed.to_string())` after the regex match. `to_string()` runs *before* `insert()` decides whether the entry is new — so duplicate import lines (very common — every chunk in a file repeats the `use ...` block) allocate a `String`, hash it, find the duplicate, then drop. For a file with 10 chunks each repeating 20 imports that's 180 wasted `String` allocs per call. `cqs where`/`cqs task` runs this on every call, with no compiled-regex caching for the seen set itself.
- **Suggested fix:** Use the standard `if !seen.contains(trimmed) { seen.insert(trimmed.to_string()); imports.push(trimmed.to_string()); }` — one alloc on first sight, zero on duplicates. Same pattern as PERF-V1.36-4 above.

#### PERF-V1.36-6: `BuildHierarchy` rebuilds `visited_names: Vec<String>` from already-interned `Arc<str>` keys
- **Difficulty:** easy
- **Location:** src/serve/data.rs:739
- **Description:** `let visited_names: Vec<String> = depth_by_name.keys().map(|s| s.to_string()).collect();` clones every Arc<str> to a fresh String. Hot path in the daemon hierarchy view — N can be ~10k for a heavy hub. Then those strings are used purely as bind-keys (`q.bind(n)`) and HashMap keys (`name_to_first_id: HashMap<String, String>`) downstream. We had Arc<str> already; SQLite bind accepts `&str` so the conversion is pointless.
- **Suggested fix:** Keep `Vec<Arc<str>>` (or `Vec<&str>` borrowed from the keys) and bind via `n.as_ref()`. For `name_to_first_id`, key by `Arc<str>` so the chain stays alloc-free. Saves ~10k String clones on a deep hierarchy.

#### PERF-V1.36-7: `search_by_names_batch` clones chunk_id and original_name strings per FTS row, even on miss
- **Difficulty:** medium
- **Location:** src/store/chunks/query.rs:436-450
- **Description:** For each light_row the inner loop does `ids_to_fetch.push(id.clone())` and `matched.push((id.clone(), original_name.to_string(), score))` — two String clones per matched row, plus `result.entry(original_name.to_string()).or_default()` which always allocates the key (even on existing entries). Phase 3 then does `chunk_row.clone()` (line 466) — full ChunkRow clone including content/doc/signature for every result row. Caller is `gather`'s `fetch_and_assemble`, hit per gather call.
- **Suggested fix:** Use `result.raw_entry_mut()` (or precompute `original_name: String` once per batch entry, share via Arc), and replace `chunk_row.clone()` with `chunk_row` move — `full_chunks` is consumed after this loop, so use `into_iter` + `HashMap::remove(&id)` instead of `get(&id).clone()`. Same pattern as `final_scored.into_iter().filter_map(rows_map.remove(&id))` at query.rs:367.

#### PERF-V1.36-8: `fresh_sentinel_nonce` calls `format!("{:02x}", b)` 16 times → 16 String allocations per LLM prompt
- **Difficulty:** easy
- **Location:** src/llm/prompts.rs:33-41
- **Description:** Generates a 32-char hex nonce by looping over 16 bytes and calling `hex.push_str(&format!("{:02x}", b))`. Each iteration allocates a 2-char String, copies into hex, drops. For batch summarization (#1108-related) where prompts are built per-chunk, this fires on every prompt — N×16 allocations. `write!` into `hex` with `core::fmt` would skip the per-byte allocations.
- **Suggested fix:** Use `std::fmt::Write::write!(&mut hex, "{:02x}", b).unwrap()` (infallible into `String`) or `hex.push(hex_char(b >> 4)); hex.push(hex_char(b & 0xf));` for zero allocs. Same pattern at src/cli/commands/io/reconstruct.rs:72 and src/cli/commands/train/train_pairs.rs:37,43 — `out.push_str(&format!(...))` is the giveaway. P3-41 from v1.33.0 was supposedly fixed (#1363) but new sites have landed since.

#### PERF-V1.36-9: `SpladeIndex::search_with_filter` clones every id pushed into the heap
- **Difficulty:** medium
- **Location:** src/splade/index.rs:248-252
- **Description:** After scoring `~min(corpus, query×256)` ≈ ~18k entries, the heap-feed loop does `heap.push(id.clone(), score)` for every (id, score) pair, regardless of whether the heap will accept the entry. With k=200 and 18k scored, that's ~17800 String clones that get immediately dropped inside `BoundedScoreHeap::push` (which always takes `String` by value). At 32-char chunk ids, ~570KB of churn per search.
- **Suggested fix:** Add `BoundedScoreHeap::push_lazy<F: FnOnce() -> String>(&mut self, score: f32, id_fn: F)` that only invokes the closure when the score crosses the eviction boundary. Then call `heap.push_lazy(score, || id.to_string())` — clones only happen for the ~k entries that survive. Same pattern would help at src/serve/data.rs:898-906 where `caller_id.clone()` and `callee_id.clone()` fire per row for the dedup HashSet.

#### PERF-V1.36-10: MMR re-rank converts `Vec<SearchResult>` → `Vec<Option<SearchResult>>` → `Vec<SearchResult>` via two extra collects
- **Difficulty:** easy
- **Location:** src/search/query.rs:435-443
- **Description:** Hot path inside `finalize_results`. To reorder by MMR picks, the code does `let originals: Vec<Option<SearchResult>> = results.into_iter().map(Some).collect::<Vec<_>>(); let mut originals = originals; for &i in &picks { ... }`. That's an extra Vec allocation (Some-wrapping every result, ~Option discriminant per element) plus a redundant rebind. With `Vec<SearchResult>` containing 100-200 results carrying full content/doc, the wrapping is ~24 bytes × N of needless allocation.
- **Suggested fix:** Use `mem::take(&mut results[i])` with `Default::default()` placeholder, OR use `Vec<MaybeUninit<SearchResult>>` and `assume_init` after picks, OR build a permutation `picks: &[usize]` and apply with `swap`-based reorder in place. Cleanest: collect picks into `BTreeMap<usize, ()>`, then drain `results.into_iter().enumerate().filter_map(|(i, r)| picks.get(&i).map(|_| r)).collect()` — one alloc total, no Option wrapping.

#### PERF-V1.36-11: `scout` looks up `stale_set` via double-allocating PathBuf-to-String round trip per file
- **Difficulty:** easy
- **Location:** src/scout.rs:284
- **Description:** `stale_set.contains(&file.to_string_lossy().to_string())` allocates twice per file (once for the `Cow<str>` from `to_string_lossy`, once for the explicit `to_string()`). On Windows the Cow is always Owned anyway. With 50 files in a scout result, that's 100 allocations purely for set lookup. `HashSet<String>` supports `contains::<str>(&str)`, so the second `to_string()` is unneeded.
- **Suggested fix:** `stale_set.contains(&*file.to_string_lossy())` or `stale_set.contains(file.to_string_lossy().as_ref())`. Drops the second alloc. For Linux (Cow::Borrowed common case) drops both.

#### PERF-V1.36-12: `vec!["?"; n].join(",")` placeholder construction allocates Vec + Vec<&str> + final String per batch
- **Difficulty:** easy
- **Location:** src/serve/data.rs:777, 871-872 (and likely other call sites)
- **Description:** Each batch builds `placeholders` via `vec!["?"; batch.len()].join(",")` — allocates a `Vec<&'static str>` of N elements, then joins. The hierarchy edge SQL (line 871-872) does this twice per inner-loop iteration, on N² batches. For deep hierarchies (visited_names > 32k) the cartesian product is many sub-queries; each pays double placeholder construction. `make_placeholders` (used in store/chunks/) skips the Vec by writing `?,?,?...` directly with `String::with_capacity(2*n)`.
- **Suggested fix:** Use the existing `crate::store::helpers::make_placeholders(n)` consistently — it's already optimized and used elsewhere. Audit grep for `vec!["?"` usages and replace with the helper.

DONE
