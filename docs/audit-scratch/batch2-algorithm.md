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

