## Algorithm Correctness

#### `semantic_diff` sort by similarity has no secondary tie-breaker — non-deterministic "most changed" ordering across runs
- **Difficulty:** easy
- **Location:** `src/diff.rs:202-207` (and the parallel test helper at `src/diff.rs:298-303`)
- **Description:** `semantic_diff` populates `modified: Vec<DiffEntry>` by iterating a `HashMap` (process-seed-randomized order) and sorts with only one key:
  ```rust
  modified.sort_by(|a, b| match (a.similarity, b.similarity) {
      (Some(sa), Some(sb)) => sa.total_cmp(&sb),
      (Some(_), None) => std::cmp::Ordering::Less,
      (None, Some(_)) => std::cmp::Ordering::Greater,
      (None, None) => std::cmp::Ordering::Equal,
  });
  ```
  Two modified entries with identical similarity (e.g., both 0.73 — common for small, nearly-identical refactors) sort into arbitrary relative order across process invocations because `sort_by` is stable w.r.t. the (HashMap-derived, random) input order, not the data. `cqs diff` and `cqs drift` JSON output will reorder identical rows between runs, defeating diff-the-diff comparisons, breaking test determinism, and making eval-flake hard to reproduce. All other score-sorting sites in the codebase carry a full `(file, name, line_start)` tie-break cascade — this one was missed in the v1.25.0 wave-1 sweep that fixed the rest.
- **Suggested fix:** Replace the `Equal` fallbacks with a cascade on the stable identity fields `DiffEntry` already carries:
  ```rust
  fn cmp_entries(a: &DiffEntry, b: &DiffEntry) -> std::cmp::Ordering {
      match (a.similarity, b.similarity) {
          (Some(sa), Some(sb)) => sa.total_cmp(&sb),
          (Some(_), None) => std::cmp::Ordering::Less,
          (None, Some(_)) => std::cmp::Ordering::Greater,
          (None, None) => std::cmp::Ordering::Equal,
      }
      .then_with(|| a.file.cmp(&b.file))
      .then_with(|| a.name.cmp(&b.name))
      .then_with(|| a.chunk_type.cmp(&b.chunk_type))
  }
  ```
  Apply to both production (line 202) and the test at line 298 so they don't drift. Add a `proptest!`-style shuffling test that asserts the sort is stable across shuffled inputs.

#### `is_structural_query` keyword probe uses `format!(" {} ", kw)` and misses keywords at end-of-query
- **Difficulty:** easy
- **Location:** `src/search/router.rs:787-789`
- **Description:**
  ```rust
  STRUCTURAL_KEYWORDS
      .iter()
      .any(|kw| query.contains(&format!(" {} ", kw)) || query.starts_with(&format!("{} ", kw)))
  ```
  Covers keywords preceded by whitespace and surrounded by whitespace (via `" {} "`) or at the very start (via `"{} "`), but **not keywords at the end of the query**. Concrete failure trace for `"find all trait"` (3 words):
  - `is_identifier_query`: `"all"` is in `NL_INDICATORS` → returns false.
  - `is_cross_language_query`: no two language names → false.
  - `extract_type_hints`: "trait" isn't in the chunk-type hint table (which is phrases like "all traits") → none returned.
  - `is_structural_query`: `STRUCTURAL_PATTERNS_AC` doesn't match; keyword loop with `kw="trait"` → `query.contains(" trait ")` false (no trailing space), `query.starts_with("trait ")` false. **All keywords fail** → false.
  - `is_behavioral_query`: no behavioral verb word-match, no "code that"/"function that" → false.
  - `is_conceptual_query`: `words.len() == 3 <= 3`, `"all"` is NL-indicator match, `!is_structural_query` → **true**.
  - Routes to `Conceptual` (α=0.70), should have been `Structural` (α=0.90).

  Same pattern for `"show me all trait"`, `"find every impl"`, `"list all enum"`, `"all class"`, `"find enum"`, etc. — i.e., the common NL pattern where a user ends their query with the type they're looking for. This shifts SPLADE α from 0.90 → 0.70 for every such query (≈20% heavier SPLADE weight than intended on Structural), and the strategy enum shifts from `DenseWithTypeHints` → `DenseDefault`, bypassing the type-boost path entirely. Also allocates a `String` per (keyword × probe) iteration on every classify. The adjacent structural-pattern check uses Aho-Corasick — the keyword path should too.
- **Suggested fix:** Replace with a word-boundary check over the pre-computed `words` vec (same approach already used for `NEGATION_TOKENS`):
  ```rust
  pub fn is_structural_query(query: &str) -> bool {
      if STRUCTURAL_PATTERNS_AC.is_match(query) { return true; }
      // words is computed once upstream; pass it through instead of re-splitting
      let words: Vec<&str> = query.split_whitespace().collect();
      STRUCTURAL_KEYWORDS.iter().any(|kw| words.iter().any(|w| w == kw))
  }
  ```
  Add regression tests: `"find all trait"` → Structural, `"all class"` → Structural, `"find enum"` → Structural. No allocation, correct at EOL, matches the pattern the rest of the router uses.

#### `bfs_expand` processes BFS seeds in HashMap iteration order — non-deterministic `name_scores` when `max_expanded_nodes` cap is reached mid-expansion
- **Difficulty:** easy
- **Location:** `src/gather.rs:317-320` (seed enqueue from `name_scores.keys()`) and `src/gather.rs:326,338` (cap checks)
- **Description:**
  ```rust
  let mut queue: VecDeque<(Arc<str>, usize)> = VecDeque::new();
  for name in name_scores.keys() {
      queue.push_back((Arc::from(name.as_str()), 0));
  }
  while let Some((name, depth)) = queue.pop_front() {
      // ...
      if name_scores.len() >= opts.max_expanded_nodes && visited.len() > initial_size {
          expansion_capped = true;
          break;
      }
      // expand neighbors
  }
  ```
  `name_scores` is a `HashMap<String, ...>`, so `name_scores.keys()` iterates in seed-randomized order. When the BFS hits `max_expanded_nodes` mid-expansion (common on dense graphs — default `max_expanded_nodes` = 50 for onboard callers BFS, see `src/onboard.rs:165`), which seeds got expanded and which got cut off depends entirely on which order the iterator handed them out. Different runs of `cqs gather`, `cqs task`, `cqs onboard` on the same corpus/query produce different expanded graphs, different score maps, different final chunk lists after dedup+truncate. This is exactly the class of non-determinism the v1.25.0 tie-break sweep targeted, but it sits one layer up in the pipeline (BFS graph seeding, not result sorting).
- **Suggested fix:** Enqueue seeds in a deterministic order — easiest is a sort by `(initial_score desc, name asc)` before push:
  ```rust
  let mut seeds: Vec<(&String, (f32, usize))> =
      name_scores.iter().map(|(k, v)| (k, *v)).collect();
  seeds.sort_by(|a, b| {
      b.1.0.total_cmp(&a.1.0)               // higher score first
          .then_with(|| a.0.cmp(b.0))        // tie on name asc
  });
  for (name, _) in seeds {
      queue.push_back((Arc::from(name.as_str()), 0));
  }
  ```
  This respects the "process higher-scoring seeds first" intent (the old code happened to do this only by coincidence of HashMap hashing), and makes the cap-at-50 cutoff deterministic. Add a test that seeds two equally-scored entries, caps at a small `max_expanded_nodes`, and asserts the same `name_scores` on 100 re-runs.

#### `llm::summary::contrastive_neighbors` top-K selection sorts by score alone — non-deterministic neighbor choice when similarities tie
- **Difficulty:** easy
- **Location:** `src/llm/summary.rs:263,265,267`
- **Description:** Three sibling sorts all use `b.1.total_cmp(&a.1)` with no tie-break:
  ```rust
  candidates.sort_unstable_by(|a, b| b.1.total_cmp(&a.1));           // line 263
  candidates.select_nth_unstable_by(limit - 1, |a, b| b.1.total_cmp(&a.1));  // line 265
  candidates.truncate(limit);
  candidates.sort_unstable_by(|a, b| b.1.total_cmp(&a.1));           // line 267
  ```
  `candidates` is `Vec<(usize, f32)>` where `usize` is an index into `valid_owned`. When multiple candidates have identical similarity (common at low precision — f32 embeddings clamp to the same bit pattern for very close vectors, especially for L2-normalized embeddings over the same reindex cohort), `select_nth_unstable` can pick any of them, and the final neighbor set for a given seed is non-deterministic. This propagates into the prompt sent to the LLM for contrastive summary generation, so the *same* corpus + *same* seed chunk produces different summaries on different runs. Contrastive summary caching by content_hash then either caches the first random result forever (good) or wastes Batches API credits regenerating when the cache misses (bad — ~$0.38/run Haiku).
- **Suggested fix:** All three sort calls need the index as a secondary key. `candidates: Vec<(usize, f32)>` already carries the index:
  ```rust
  candidates.sort_unstable_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
  candidates.select_nth_unstable_by(limit - 1, |a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
  candidates.truncate(limit);
  candidates.sort_unstable_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
  ```
  Same cascade the rest of the codebase applies everywhere else.

#### `--name-boost` CLI arg accepts negative / >1 values — negative embedding weight, out-of-range fusion
- **Difficulty:** easy
- **Location:** `src/cli/args.rs:57-58` (arg declaration); `src/search/scoring/candidate.rs:286` (consumer)
- **Description:** CLI argument validation:
  ```rust
  #[arg(long, default_value = "0.2", value_parser = parse_finite_f32)]
  pub name_boost: f32,
  ```
  `parse_finite_f32` only rejects NaN/Infinity; any other `f32` value passes through. Consumer in `apply_scoring_pipeline`:
  ```rust
  (1.0 - ctx.filter.name_boost) * embedding_score + ctx.filter.name_boost * name_score
  ```
  A user calling `cqs search "foo" --name-boost 5.0` gets `(1.0 - 5.0) * embedding = -4.0 * embedding_score`, i.e., the embedding signal is **negated** — identical semantic matches get ranked last. Symmetrically, `--name-boost -1.0` gives `2.0 * embedding - 1.0 * name_score`, over-weighting embedding past its natural [0,1] range. The `.clamp(0.0, 1.0)` that the config-file path applies at `src/config.rs:370-371` is not mirrored on the CLI-flag path, so a config that looked safe can be overridden into a search-breaking regime via a stray flag. Most eval scripts set `--name-boost` explicitly, so a typo is one `bash` run away.
- **Suggested fix:** Replace the argument parser with a clamped variant. Either add a helper:
  ```rust
  fn parse_name_boost(s: &str) -> std::result::Result<f32, String> {
      let v = parse_finite_f32(s)?;
      if (0.0..=1.0).contains(&v) { Ok(v) } else {
          Err(format!("name_boost must be in [0.0, 1.0], got {v}"))
      }
  }
  ```
  and use `value_parser = parse_name_boost` at line 57. Or enforce the clamp at `SearchFilter` construction so config and CLI paths converge. Same fix applies to any other weight/threshold-style f32 flag.

#### `reranker::compute_scores_opt` — `batch_size * stride` unchecked multiplication hides shape errors; `data[i * stride]` can panic on overflow
- **Difficulty:** easy
- **Location:** `src/reranker.rs:368-387`
- **Description:**
  ```rust
  let stride = if shape.len() == 2 { shape[1] as usize } else { 1 };
  if stride == 0 { /* return error */ }
  let expected_len = batch_size * stride;              // <-- unchecked mul
  if data.len() < expected_len { /* return error */ }
  let scores: Vec<f32> = (0..batch_size).map(|i| sigmoid(data[i * stride])).collect();
  ```
  `shape[1]` is `i64` from ORT. The zero-guard landed after the prior audit (RB-8) but the negative-dim and overflow guards are still missing:
  - `shape[1] = -1` → `(-1_i64 as usize) = usize::MAX` (on 64-bit).
  - `batch_size * usize::MAX` wraps to a small value; `data.len() < expected_len` passes with that small wrapped value.
  - Inside the loop, `i * stride` also wraps, indexing `data` at an arbitrary position. If the wrapped index exceeds `data.len()`, **Rust bounds-checks and panics** in the middle of a hot inference call — aborting the entire search pipeline.
  A malicious / corrupted ONNX file (or a new reranker with an unusual output tensor layout) is the reachable source of a negative or pathologically-large `shape[1]`.
- **Suggested fix:** Guard the cast and the multiply:
  ```rust
  if shape.len() == 2 && shape[1] <= 0 {
      return Err(RerankerError::Inference(format!(
          "reranker output has non-positive dim 1: {}", shape[1]
      )));
  }
  let stride = if shape.len() == 2 { shape[1] as usize } else { 1 };
  if stride == 0 { /* existing error */ }
  let expected_len = batch_size.checked_mul(stride).ok_or_else(|| {
      RerankerError::Inference(format!(
          "reranker expected_len overflows: batch_size={batch_size} stride={stride}"
      ))
  })?;
  if data.len() < expected_len { /* existing error */ }
  ```
  Same pattern fixes the SPLADE six-site parallel in `splade/mod.rs` (see prior audit RB-9). The `data[i * stride]` indexing can stay as-is once the upstream `expected_len` check is sound.

#### `llm::doc_comments::select_uncached` sort has no tie-break beyond content length — non-deterministic selection when `max_docs` truncates
- **Difficulty:** easy
- **Location:** `src/llm/doc_comments.rs:222-229,242`
- **Description:**
  ```rust
  uncached.sort_by(|a, b| {
      let a_no_doc = a.doc.as_ref().is_none_or(|d| d.trim().is_empty());
      let b_no_doc = b.doc.as_ref().is_none_or(|d| d.trim().is_empty());
      b_no_doc.cmp(&a_no_doc)
          .then_with(|| b.content.len().cmp(&a.content.len()))
  });
  // ...
  uncached.truncate(uncached_cap);
  ```
  Two chunks with the same `has-doc` status and the same content-length byte count collide on the compare; `sort_by` is stable w.r.t. the input `uncached` vec's order, which is fed by a DB scan that may return duplicates-by-size in any order depending on index layout. When `--improve-docs --max-docs N` trips the truncate (line 242), which rows get documented vs skipped is non-deterministic across runs. For a Claude Batches API call (≈ $0.38 / run Haiku), that means the set of chunks that eat budget is non-reproducible. Between the enrichment re-run and the contrastive-summaries batcher this is the third "tie-break missing" site in `llm/*.rs`.
- **Suggested fix:** Append a stable tertiary key — chunk id is always unique and carried by `ChunkSummary`:
  ```rust
  .then_with(|| b.content.len().cmp(&a.content.len()))
  .then_with(|| a.id.cmp(&b.id))
  ```

#### `token_pack` breaks on first oversized item — drops smaller items that would fit, undershoots budget
- **Difficulty:** easy
- **Location:** `src/cli/commands/mod.rs:398-417` (greedy loop in `token_pack`)
- **Description:** The greedy knapsack loop treats budget overflow as a hard stop:
  ```rust
  for idx in order {
      let tokens = token_counts[idx] + json_overhead_per_item;
      if used + tokens > budget && kept_any {
          break;          // <-- should be `continue;`
      }
      // ...
      used += tokens;
      keep[idx] = true;
  }
  ```
  Once a single item fails to fit, the loop exits — every lower-scored item is dropped, even items that would comfortably fit in the remaining budget. Concrete repro: budget = 300, items sorted by score descending = `[A=250 tokens, B=100 tokens, C=40 tokens]`. After `A` is packed (used=250), `B` fails (`350 > 300`) → `break` → `C` is silently dropped, even though `used + 40 = 290 ≤ 300`. With `continue`, `C` would land in the result and the function would return `(2 items, 290 tokens)` instead of `(1 item, 250 tokens)`. Hits every consumer of `--tokens` — `cqs context`, `cqs explain`, `cqs scout`, `cqs gather`, `cqs task`, the CLI/batch search packers, etc. — under the realistic mix where one large chunk is followed by smaller chunks in the score-ordered list. Particularly bad for code search where high-relevance fixtures (whole modules) often outweigh the per-symbol chunks that would otherwise round out the response.
- **Suggested fix:** Replace `break` with `continue` so the loop keeps probing for fits, and drop the now-redundant `kept_any` short-circuit on the break (the `kept_any && tokens > budget` check is still needed for the "include at least one" branch). Add a regression test with score-sorted items `[oversized, fits, fits]` asserting the two `fits` survive.

#### `map_hunks_to_functions` returns hunks in HashMap iteration order — non-deterministic `cqs impact-diff` JSON across runs
- **Difficulty:** easy
- **Location:** `src/impact/diff.rs:38-106` (`map_hunks_to_functions` outer loop), and the downstream truncation at `src/impact/diff.rs:154-168`
- **Description:** Two layered determinism bugs in the diff-impact pipeline:
  1. `by_file: HashMap<&Path, Vec<&DiffHunk>>` is iterated at line 66 (`for (file, file_hunks) in &by_file`). HashMap iteration is process-seed-randomized, so the order of `functions: Vec<ChangedFunction>` produced is run-to-run random for any diff that touches more than one file.
  2. The `analyze_diff_impact_with_graph` cap at line 165 uses `changed.into_iter().take(cap)` (default cap = 500). When the input exceeds 500 functions, *which* 500 survive depends on the random Vec order from step 1 — so on a real "big refactor" diff (>500 changed functions), `cqs impact-diff` output is nondeterministically truncated. Two runs against the same diff give different `changed_functions`, different caller batches, different reverse-BFS results, different test sets, different `via` attributions.
- **Suggested fix:** Sort `changed` by `(file_path, line_start, name)` after `map_hunks_to_functions` builds it, before the cap takes effect:
  ```rust
  let mut changed = map_hunks_to_functions(...);
  changed.sort_by(|a, b| {
      a.file.cmp(&b.file)
          .then(a.line_start.cmp(&b.line_start))
          .then(a.name.cmp(&b.name))
      });
  ```
  Or build `by_file` as a `BTreeMap`/`Vec<(&Path, …)>` sorted by path. Add a regression test with a diff spanning 3 files and assert `functions` is identical across 100 calls.

#### `drain_pending_rebuild` dedup against rebuild-thread snapshot drops fresh embeddings for chunks whose content changed during the rebuild window
- **Difficulty:** medium
- **Location:** `src/cli/watch.rs:1077-1105` (`drain_pending_rebuild`, the `known` filter)
- **Description:** The non-blocking HNSW rebuild added in #1113 streams a snapshot of `(id, embedding)` from a read-only Store handle in a worker thread, while the watch loop continues capturing newly upserted `(id, embedding)` pairs into `pending.delta`. On swap, the code dedups via:
  ```rust
  let known: HashSet<&str> = new_index.ids().iter().map(String::as_str).collect();
  let to_replay: Vec<(String, Embedding)> = pending.delta
      .into_iter()
      .filter(|(id, _)| !known.contains(id.as_str()))
      .collect();
  ```
  `known` contains every chunk id the rebuild thread saw at its snapshot moment. If the watch loop *re-embedded* a chunk during the rebuild window (file edit while rebuild was in flight — exactly the case the non-blocking rebuild was added to handle), the new `(id, Embedding)` pair lands in `pending.delta`, but `known` already contains the id with the *old* embedding. The filter drops the fresh embedding, and the swapped-in HNSW carries the stale vector until the next threshold rebuild. For an editor save loop this means up to 100 saves' worth of stale vectors against the freshly-modified file, exactly defeating the rebuild's purpose. Search for the modified file returns hits on the pre-edit content.
- **Suggested fix:** Dedup must compare the embedding payload, not just the id. Cleanest: have the rebuild thread return `Vec<(String, blake3_hash)>` alongside the index, and replay any delta whose `(id, hash)` differs from the snapshot. Cheaper alternative: use the chunk's `content_hash` from the store at delta-capture time — `pending.delta` becomes `Vec<(String, Embedding, ContentHash)>`, and the dedup filter checks the content hash matches the rebuilt vector's hash. Add a test that mid-rebuild-window upserts of an existing id produce the *new* embedding in the swapped-in index.

#### `search_reference` weighted threshold filters using post-weight comparison but pre-weight `limit` — multi-ref ranking truncates valid candidates
- **Difficulty:** easy
- **Location:** `src/reference.rs:231-258` (`search_reference`, `apply_weight = true` branch)
- **Description:** The flow is:
  ```rust
  let mut results = ref_idx.store.search_filtered_with_index(
      query_embedding, filter, limit, threshold, ref_idx.index.as_deref(),
  )?;
  if apply_weight {
      for r in &mut results { r.score *= ref_idx.weight; }
      results.retain(|r| r.score >= threshold);
  }
  ```
  Two coupled algorithm bugs:
  1. `search_filtered_with_index` is asked for the top `limit` results that satisfy `score ≥ threshold`. With `weight = 0.8` and a `threshold = 0.5`, a chunk that scored 0.62 (above raw threshold) will pass into `results`, then become 0.50 after `*= weight`. A chunk that scored 0.40 (below raw threshold) is dropped by the underlying search — but `0.40 * 1/weight = 0.50` would have been the boundary. The reference therefore *systematically* under-samples its own corpus when `weight < 1.0`: the underlying top-`limit` cap is computed against unweighted scores, missing valid post-weight survivors.
  2. The post-weight `retain` then re-applies the same `threshold` on weighted scores, double-filtering: a 0.51 raw score (passes raw threshold) becomes 0.408 weighted (fails weighted threshold), so it's dropped *after* the underlying search already spent the cycle on it. The user pays for the search but gets a smaller result set than `limit` even when more candidates exist.
- **Suggested fix:** When `apply_weight` is true, query the underlying store with a relaxed threshold (`threshold / weight` for weight > 0) and an over-fetch limit, then weight + filter + sort + truncate to `limit` in caller-side code:
  ```rust
  let raw_threshold = if apply_weight && ref_idx.weight > 0.0 {
      threshold / ref_idx.weight
  } else { threshold };
  let raw_limit = if apply_weight {
      // 2x or 3x over-fetch leaves headroom for the weighted retain step
      (limit as f32 * 2.0).ceil() as usize
  } else { limit };
  let mut results = ref_idx.store.search_filtered_with_index(
      query_embedding, filter, raw_limit, raw_threshold, ref_idx.index.as_deref())?;
  if apply_weight {
      for r in &mut results { r.score *= ref_idx.weight; }
      results.retain(|r| r.score >= threshold);
      results.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.chunk.id.cmp(&b.chunk.id)));
      results.truncate(limit);
  }
  ```
  Same fix shape applies to `search_reference_by_name` at line 265-285 (which has the threshold-then-weight ordering inverted, hiding the same bug).

#### `find_type_overlap` chunk_info dedup picks `(file, line)` from random HashMap iteration — non-deterministic `cqs related` per-result file attribution
- **Difficulty:** easy
- **Location:** `src/related.rs:131-147` (build of `chunk_info`); `src/related.rs:155-157` (final sort, no tie-break on name)
- **Description:** Two algorithm bugs in the same loop:
  1. `chunk_info: HashMap<String, (PathBuf, u32)>` uses `or_insert(...)` to remember the first `(file, line)` seen for each function name. The outer iteration `for chunks in results.values()` walks `results: HashMap<String, Vec<ChunkSummary>>` in process-seed-random order. When a function name appears across multiple type result lists (common — a function uses several types and so shows up in each type's user list), the `or_insert` retains the first arrival, which depends on which type's bucket happens to be first in the random iteration. For a function defined in one file but with overloads or test fixtures in another, the result row's `file` field flips run-to-run.
  2. The final sort at line 155-157 is `sorted.sort_by_key(|e| Reverse(e.1))` (count only) followed by `truncate(limit)`. Counts in this domain are tiny integers (1, 2, 3) and equal counts are the rule, not the exception. Truncate then picks arbitrary names from a HashMap-ordered Vec.
  3. Earlier at line 59-65, `type_names` is computed via `HashSet → into_iter().collect::<Vec>()` — also random order — though this only affects bind ordering downstream, not result identity.
  Net effect: `cqs related <fn>` returns different `shared_types` lists across runs, with different `(file, line)` attribution per result. Defeats `cqs related <fn>` reproducibility for evals or cached agent prompts.
- **Suggested fix:** (a) Sort `type_counts.into_iter()` results into a deterministic Vec before the count-sort: `sorted.sort_by(|a, b| Reverse(a.1).cmp(&Reverse(b.1)).then(a.0.cmp(&b.0)))` so equal counts break by name asc. (b) For `chunk_info`, walk `results` in sorted-by-key order so the first `or_insert` is deterministic — or store *all* `(file, line)` candidates per name and pick `min` by `(file, line)` after the loop. (c) Convert the HashSet collect at line 63-65 into a sort: `let mut type_names: Vec<_> = ...; type_names.sort(); type_names.dedup();`.

#### CAGRA `search_with_filter` silently under-fills when `included < k` — caller cannot distinguish "few matching candidates" from "filter too restrictive"
- **Difficulty:** medium
- **Location:** `src/cagra.rs:520-598` (`search_with_filter`); `src/cagra.rs:344-486` (`search_impl`)
- **Description:** When the caller asks for `k` results filtered by a predicate, the bitset path does:
  ```rust
  let mut included = 0usize;
  for (i, id) in self.id_map.iter().enumerate() {
      if filter(id) { bitset[i / 32] |= 1u32 << (i % 32); included += 1; }
  }
  if included == n { return CagraIndex::search(self, query, k); }
  if included == 0 { return Vec::new(); }
  // else: ask CAGRA for k — but only `included` slots can ever be filled
  ```
  When `included < k` (e.g., `cqs search "foo" --include-type Function --lang rust` over a corpus where the Function/Rust subset has 12 vectors but `k = 20`), CAGRA receives `topk = 20` and writes valid `(neighbor, distance)` pairs into the first `included` slots; the remaining `k - included` slots stay at the `INVALID_DISTANCE` sentinel. The `!dist.is_finite()` check at line 473 correctly drops those slots, but the caller above this layer (e.g., `Store::search_filtered_with_index`) sees `Vec<IndexResult>` of length `min(included, k)` with no signal that under-fill happened. Downstream paging / pagination logic that assumes "got fewer than k → end of results" is correct, but a user who set `--limit 20` and gets 12 has no way to distinguish "this filter combination has only 12 hits" from "CAGRA itopk_size cap silently truncated".

  Worse, when `included < k` AND `k > itopk_max` (#988 reported `itopk_size_max=480` while `k=500` failed), CAGRA returns an error from `gpu.index.search(...)` and `search_impl` logs at error then returns `Vec::new()` — silently zeroing out a query that, with `k = included`, would have succeeded. The path doesn't try a fallback with `k = included.min(k)`.
- **Suggested fix:** Cap `k` at `included` before invoking `search_impl` so CAGRA is always asked for a feasible top-K:
  ```rust
  let effective_k = k.min(included);
  // … then
  self.search_impl(&gpu, query, effective_k, Some(&bitset_device))
  ```
  Add a debug log when `effective_k < k` so eval scripts can see the truncation. As a follow-on, `search_filtered_with_index` should propagate a `degraded` / `truncated` boolean upward when the underlying index returned `< k` results so that the JSON envelope can carry it (matches the pattern used in `analyze_diff_impact_with_graph` for `truncated: bool`).

#### Hybrid SPLADE fusion: `alpha == 0` branch produces unbounded `1.0 + s` scores that mix into a [-1, 1] cosine pool — magic-constant cliff at the SPLADE boundary
- **Difficulty:** easy
- **Location:** `src/search/query.rs:649-672` (the fusion lambda inside `splade_fuse_with_dense`)
- **Description:** The `alpha <= 0.0` branch ("pure re-rank mode") emits:
  ```rust
  let score = if alpha <= 0.0 {
      if s > 0.0 { 1.0 + s } else { d }
  } else {
      alpha * d + (1.0 - alpha) * s
  };
  ```
  - `s` is normalized to `[0, 1]` by dividing by `max_sparse` (line 614), but `1.0 + s` is in `[1.0, 2.0]`.
  - `d` (dense cosine) is in `[-1, 1]` (`cosine_similarity` is not clamped here — `apply_scoring_pipeline`'s `.max(0.0)` runs *later*, after this fusion).
  - A SPLADE-found chunk with `s = 0.001` (barely-there sparse signal — possible when its single shared subword token barely fires) gets `1.0 + 0.001 = 1.001`, which beats *every* SPLADE-unknown chunk regardless of how relevant they are by dense cosine (best possible 1.0 unclamped).
  - A SPLADE-found chunk with `s = 0` is *not* in `sparse_scores` because `sparse_scores.insert(&r.id, normalized)` runs even when `normalized = 0` only if the source had `r.score > 0` upstream — confirmed (and `max_sparse > 0` gate at line 614). So the `s > 0` test does what it says, but the cliff at the threshold (any positive sparse hit, no matter how small, dominates dense) is a hidden gotcha. Real-world impact: setting `--alpha 0` (re-rank mode) inverts the result list when a sparse-only weak match exists.
- **Suggested fix:** Use a calibrated additive boost rather than a magic constant — e.g., `let boost = 1.0 + s * 0.1;` would still place SPLADE-found chunks above any non-found candidate while preserving their dense ordering relative to each other within the boost band. Or, as the cleaner option, treat `alpha == 0` symmetrically with the linear path: `0.0 * d + 1.0 * s = s`, and rely on an `s > d` post-filter to bias toward sparse hits. Either way, drop the `1.0 + s` magic constant — it inflates SPLADE matches into a band the dense path can never reach. Add a doc-test repro: dense pool `[(A, 0.95)]`, sparse pool `[(B, 0.001 normalized)]`, `alpha = 0.0` → expect `A` first, but the current code returns `[B, A]` with `B@1.001`.

#### `apply_scoring_pipeline` / hybrid path drops embedding-score sign without clamping — negative cosine inflates negatives via `name_boost` sign-flip
- **Difficulty:** easy
- **Location:** `src/search/scoring/candidate.rs:283-298` (the `(1.0 - name_boost) * embedding_score + name_boost * name_score` blend)
- **Description:**
  ```rust
  let base_score = if let Some(matcher) = ctx.name_matcher {
      let n = name.unwrap_or("");
      let name_score = matcher.score(n);
      (1.0 - ctx.filter.name_boost) * embedding_score + ctx.filter.name_boost * name_score
  } else { embedding_score };
  // …
  let mut score = base_score.max(0.0) * ctx.note_index.boost(file_part, chunk_name);
  ```
  `embedding_score` here is raw cosine which is in `[-1, 1]` for un-normalized or oddly-normalized vectors and even for unit-norm vectors when the chunk and query are anti-correlated. The blend `(1 - nb) * e + nb * ns` with `e = -0.3, ns = 0.0, nb = 0.2` produces `-0.24`. The subsequent `.max(0.0)` clamps to `0.0`, which then misses the `score >= threshold` test (`threshold` defaults > 0). So far so good for the negative case.
  But when `name_boost > 1` is supplied (CLI accepts arbitrary finite `f32`, see the still-pending AC-V1.29-5 from triage), `(1 - nb)` is negative, multiplying `e = 0.9` (a great match) by `-0.5` and adding `nb * ns`. The `.max(0.0)` then turns this into `0.0`, silently demoting good matches to zero. Net effect: an out-of-range `--name-boost` flag does not just mis-weight — it deletes good results. Compounds with the same finding in the existing scratch (#5).
- **Suggested fix:** Clamp `name_boost` to `[0.0, 1.0]` at `SearchFilter` construction (single fix point that closes both the CLI and config paths), and clamp `embedding_score` to `[0.0, 1.0]` *before* the blend so the linear interpolation is always between two numbers in the same range and never produces a sign-flip:
  ```rust
  let embedding_score = embedding_score.clamp(0.0, 1.0);
  let nb = ctx.filter.name_boost.clamp(0.0, 1.0);
  let base_score = if let Some(matcher) = ctx.name_matcher {
      let name_score = matcher.score(name.unwrap_or(""));
      (1.0 - nb) * embedding_score + nb * name_score
  } else { embedding_score };
  ```
  Add a property test asserting `apply_scoring_pipeline` output ∈ `[0.0, ∞)` for *any* `name_boost`, `embedding_score`, `name_score` ∈ `f32::finite()`.

