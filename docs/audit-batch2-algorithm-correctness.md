# Algorithm Correctness — v1.38.x audit

Scope: post-#1456 / v1.38.0 work. Recent merges in focus: #1502 (HNSW Vec<Box<str>>),
#1505 (drift check), #1507 (project search filter merge), #1508 (exhaustive match),
#1509 (try_classify chain), #1510 (prompt envelope), #1511 ([index.policy] resolution).

Closed AC items from prior audits are NOT re-reported. Targeted at **algorithmic** /
boundary / off-by-one / sort-order issues introduced or surviving in the recent diffs.

---

#### AC-V1.38-1: `BoundedScoreHeap::would_accept` violates the tied-score id-tiebreak invariant — silently loses smaller-id boundary candidates in SPLADE search
- **Difficulty:** easy
- **Location:** `src/search/scoring/candidate.rs:234-247` (`would_accept`); caller in `src/splade/index.rs:253-258`
- **Description:** PERF-V1.36-9 added `would_accept` as a pre-flight gate so SPLADE can skip cloning chunk-id strings for candidates that won't enter the heap. The contract `BoundedScoreHeap` itself documents (`candidate.rs:142-149`, the AC-V1.25-1/2 fix) is "deterministic on id ascending — when scores are equal, the smaller id wins." But the new gate uses strict `score.total_cmp(worst_score).is_gt()` for the at-capacity branch, which returns `false` on tied scores. The follow-up `push()` would still evict on `id < worst_id` for that tie — but `would_accept == false` means the SPLADE caller `continue`s and never even calls `push()`. Net effect: **at the eviction boundary, a smaller-id incoming chunk that should evict the largest-id heap entry is silently dropped instead.** The bug only triggers when (a) the heap is full, (b) two SPLADE candidates have *exactly* the same accumulated dot-product score, and (c) one of them has a smaller chunk_id than the worst current entry. Tied dot-products are common at small k against large posting lists, so this is exercised more than it sounds. Pre-PERF-V1.36-9 the SPLADE path went straight to `push`, which respected the invariant.

  The bonus footgun: `would_accept(any)` returns `true` when `capacity == 0` (because `peek()` is None and the `else { true }` arm fires), but `push` silently rejects everything at capacity 0. The mismatch is harmless for SPLADE (caller never builds k=0), but any future caller that trusts `would_accept`'s return value can't.
- **Suggested fix:** In the at-capacity arm, mirror `push`'s comparator exactly: `match score.total_cmp(worst_score) { Greater => true, Equal => id_would_be_less, Less => false }`. Since `would_accept` doesn't know the incoming id, either (a) take `id: &str` as a second parameter and do the full tiebreak, or (b) return `true` for the Equal case and accept that ~1 cheap clone per tied score makes it through to be evaluated by `push`. Option (b) is the smaller diff and preserves the determinism invariant. Add a regression test against the SPLADE caller path with constructed tied scores reaching the boundary in reverse-id order.

---

#### AC-V1.38-2: `mmr_rerank` tie-break uses raw `f32 ==` instead of `total_cmp` — same anti-pattern AC-V1.30.1-7 fixed in `BoundedScoreHeap`
- **Difficulty:** easy
- **Location:** `src/search/mmr.rs:94-97`
- **Description:** The MMR per-candidate selection loop picks the best by `if mmr > best_mmr || (mmr == best_mmr && best_idx == usize::MAX)`. The `==` on raw `f32` is the exact pattern AC-V1.30.1-7 retired in `BoundedScoreHeap::push` (PR #1239). Two issues: (1) on NaN scores both `>` and `==` return `false`, so a NaN MMR silently skips that candidate and the loop may exit with `best_idx == usize::MAX`, returning fewer than `limit` results without any log or warning. (2) The intent ("only initialize on first iter") is encoded as `best_idx == usize::MAX`, which means subsequent ties never replace the first-seen winner — that's deterministic only because `i` iterates ascending. If the candidate slice is later sorted differently upstream (or if the MMR function is reused on a HashMap-derived iterator), the tie-break stops being i-ascending and the result becomes process-seed-randomized.
- **Suggested fix:** Replace the float comparison with `total_cmp`: `match mmr.total_cmp(&best_mmr) { Greater => true, Equal => best_idx == usize::MAX, Less => false }`. Add `debug_assert!(mmr.is_finite())` after the score computation so a NaN candidate score (which would imply an upstream bug) is loud rather than silent, and document that the tie-break depends on the input slice ordering.

---

#### AC-V1.38-3: `bfs_expand` seed sort uses `partial_cmp().unwrap_or(Equal)` instead of `total_cmp`
- **Difficulty:** easy
- **Location:** `src/gather.rs:323-329`
- **Description:** AC-V1.29-3 made `bfs_expand` sort its seed queue by `(score desc, name asc)` so the BFS expansion order was deterministic across HashMap seed iteration. The implementation uses `b_score.partial_cmp(a_score).unwrap_or(Ordering::Equal)`. For finite scores this works, but: (1) a NaN score (result of an upstream `cosine_similarity` against a degenerate vector — possible if an enriched embedding base wasn't recomputed) makes `partial_cmp` return `None`, the seed becomes "equal to everyone", and the secondary `name asc` tiebreak takes over — but the position depends on where in the input slice the NaN sits relative to other equal-tagged entries, which is not stable under sort. (2) `total_cmp` is what the rest of the search/scoring pipeline standardised on (`BoundedScoreHeap`, `apply_parent_boost` re-sort, `search_across_projects` merge); `bfs_expand` is the lone outlier still using `partial_cmp.unwrap_or(Equal)`.
- **Suggested fix:** `b_score.total_cmp(a_score).then_with(|| a_name.cmp(b_name))`. Same change pattern as PR #1239 applied across the rest of the codebase.

---

#### AC-V1.38-4: `try_classify_negation` priority 1 fires on bare common nouns ("no", "exclude") that aren't actually negation context
- **Difficulty:** medium
- **Location:** `src/search/router.rs:960-975` (`try_classify_negation`); token list `src/search/router.rs:377-388`
- **Description:** The negation classifier sits at priority 1 — above identifier lookup, cross-language, type-filtered, and structural — and fires on any whitespace-split token that hits the set `{not, without, except, never, avoid, no, don't, doesn't, shouldn't, exclude}`. The set includes plain English particles that appear inside completely non-negation queries: `"no"` is a single-token answer or a placeholder; `"exclude"` and `"avoid"` appear in identifier names or doc-string-style queries (`"exclude_test_files function"`, `"avoid contention"`); `"except"` is a Python keyword. Concrete misroute: `cqs "exclude_test pattern"` → tokens `["exclude_test", "pattern"]` → no hit (because the token is `exclude_test` not `exclude`), but `cqs "exclude tests"` → tokens `["exclude", "tests"]` → hits Negation, routes to `DenseBase` with `α=Negation`'s value, and the operator's "find code that excludes tests" intent is treated as "find tests, then negate" against the wrong index. The `try_classify_*` refactor in #1509 made each classifier independently testable but didn't add a context check.
- **Suggested fix:** Two-arm gate: only fire if a negation token appears AND there are ≥2 tokens after it OR there's a non-negation keyword before it. Equivalently: require the negation token to function as a connective (e.g., `query.contains(" without ")`, `query.contains(" except ")`) rather than appearing alone or at the end. Add adversarial tests `classify("exclude tests")`, `classify("avoid lock contention")`, `classify("no panic")` asserting they DON'T classify as Negation.

---

#### AC-V1.38-5: `resolve_splade_alpha` global-env arm silently drops parse errors / non-finite values that the per-cat arm warns about
- **Difficulty:** easy
- **Location:** `src/search/router.rs:783-796` (global env match); compare with per-cat arm at lines 745-781
- **Description:** The per-category SPLADE α env (`CQS_SPLADE_ALPHA_<CAT>`) match has three explicit arms: `Ok(val) if let Ok(alpha)` with non-finite warning, `Ok(val)` with parse-error warning, and explicit `Err(NotPresent)` / `Err(NotUnicode)` arms (EH-V1.36-9 added the latter). The global `CQS_SPLADE_ALPHA` arm collapsed all of those into `_ => {}`: a malformed `CQS_SPLADE_ALPHA=NaN`, `CQS_SPLADE_ALPHA=foo`, or non-unicode value falls through to slot/default with **no warning at all**. Operator who typoed `CQS_SPLADE_ALPHA=O.7` (capital O) gets the per-cat default silently and chases an A/B that doesn't reflect the env they thought was active. This isn't a silent corruption (the default is reasonable) but it's an observability gap masquerading as algorithm correctness — the algorithm "fall back to default on bad input" is the same in both arms, but only the per-cat arm tells you it happened.
- **Suggested fix:** Mirror the per-cat arm structure: explicit `Ok(val)` with parse-fallback warning, explicit `Err(VarError::NotUnicode)` with warning, `Err(VarError::NotPresent)` silent. Or factor both into a shared helper since the parse-clamp-warn-return pattern is otherwise identical.

---

#### AC-V1.38-6: `apply_parent_boost` cap clamp can overshoot `parent_boost_cap` by ~1 ULP when `(cap - 1.0) / per_child` is not exact in f32
- **Difficulty:** easy
- **Location:** `src/search/scoring/candidate.rs:75-77,103-106`
- **Description:** AC-V1.25-4 closed this once by changing `count` clamping. The current implementation computes `max_children = (cfg.parent_boost_cap - 1.0) / cfg.parent_boost_per_child` and then `boost = 1.0 + per_child * (count as f32 - 1.0).min(max_children)`. For most config values the round-trip is exact (e.g. cap=1.15, per=0.05 → max_children=3.0 by chance), but operator-overrides like `parent_boost_cap = 1.20, parent_boost_per_child = 0.03` produce `max_children = 0.20/0.03 ≈ 6.6666665`; the multiplied-back value `0.03 * 6.6666665 ≈ 0.2000` is within ULP of 0.2 but can overshoot. With count ≥ max_children+1, the resulting boost is `1.0 + 0.03 * 6.6666665 ≈ 1.2000000476...` — strictly greater than the documented cap. Sort-stability tests don't catch this because both pre- and post-boost scores are still well-ordered; the residual is a documentation/contract violation.
- **Suggested fix:** One extra clamp on the final value: `let boost = (1.0 + per_child * ...).min(cfg.parent_boost_cap);`. Costs one f32.min, fixes the ULP overshoot, and matches what the doc string at `candidate.rs:50-52` already promises ("capped at `parent_boost_cap`").

---

#### AC-V1.38-7: `from_preset(unknown_name)` silently falls back to default model — `cqs index --model typo` against an existing index runs to completion against the wrong model with no warning
- **Difficulty:** medium
- **Location:** `src/embedder/models.rs:643-662` (`ModelConfig::resolve`); interaction with drift check at `src/cli/commands/index/build.rs:1497-1525` (`check_index_model_drift`)
- **Description:** PR #1505 added `check_index_model_drift` to catch the silent dim-mismatch footgun where `cqs index --model X` against a Y-built index would feed X-dim vectors into a Y-dim store. But `ModelConfig::resolve` short-circuits unknown CLI model names: `if let Some(cfg) = Self::from_preset(name) { return cfg; } tracing::warn!(...); return Self::default_model();`. So `cqs index --model bgelarge` (typo, missing dash) emits a `tracing::warn!` (rarely visible at the operator's default log level) and resolves to the project default, which is `bge-large`. If the existing index is also `bge-large`, the drift check passes — operator believes they switched models, the index keeps building against the original. Worse case: the index is `embeddinggemma-300m`, the operator typoed `--model bge-large-en` (similar but not the preset name `bge-large`), default is `bge-large` (768-dim vs gemma's 768-dim — same dim, different vocab) → drift check fires correctly. But if defaults align with the existing index, the typo is invisible.
- **Suggested fix:** Either (a) make `cqs index --model X` hard-fail on unknown preset (don't fall back to default — the operator typed an explicit value, honour it as a request not a hint), or (b) elevate the existing `tracing::warn!` to an `eprintln!` so the typo is visible regardless of log filter, plus add an `unknown_preset = true` field to the warn and assert in `check_index_model_drift` that no unknown-preset fallback happened. The existing fallback-to-default behaviour is the safe choice for `cqs <query>` and similar read paths, but `cqs index --model` is an operator-driven write path where silence is wrong.

---

#### AC-V1.38-8: `is_vendored_origin` config entries with slashes silently never match — operator override gets ignored without warning
- **Difficulty:** easy
- **Location:** `src/vendored.rs:53-60`; config consumer in `src/cli/commands/index/build.rs:438-443`
- **Description:** `is_vendored_origin` matches each forward-slash-separated segment of the origin path against the prefix list with strict `==`. The doc says "entries should be bare directory names without slashes (the default list satisfies that contract)" — but `effective_prefixes` happily accepts whatever an operator puts in `.cqs.toml`'s `[index].vendored_paths`. If the operator writes `vendored_paths = ["vendor/oss-lib", "third_party/protobuf"]` expecting sub-path matching (a perfectly reasonable mental model — the config name is "vendored *paths*", not "vendored *segments*"), every entry with a slash in it is dead config: a single segment `"vendor"` from `vendor/oss-lib/foo.rs` will never `==` the multi-segment string `"vendor/oss-lib"`. No validation, no warning at load — vendor-tagging just silently fails for those overrides.
- **Suggested fix:** At `effective_prefixes` resolution time, validate each override entry with `if entry.contains('/') { tracing::warn!(?entry, "vendored_paths entry contains '/' and will never match — use a bare directory segment"); }` and either drop the entry or accept it but log the no-op. Even better: support multi-segment entries by checking if the origin contains `/{entry}/` or starts with `{entry}/` — both modes are useful and the function name doesn't constrain to either.

---

#### AC-V1.38-9: HNSW `ef_search` integer overflow when `k * 2` exceeds usize bounds
- **Difficulty:** easy
- **Location:** `src/hnsw/search.rs:93-94`
- **Description:** `let ef_search = self.ef_search.max(k * 2).min(index_size);` — `k * 2` is unchecked. On 64-bit usize, `k > usize::MAX / 2` will panic (unsigned overflow on `k * 2` is undefined in release builds without `-C overflow-checks`, panics in debug). Realistically the CLI bounds k via `--limit` parsing, but `search_filtered` is a public API on `HnswIndex` and callers are not contractually obligated to bound k. A daemon client request that smuggled in `k = usize::MAX - 1` would overflow. The HNSW author's defensive `.min(index_size)` covers the underlying library's k-vs-size sanity but not the intermediate computation. `saturating_mul` is the obvious patch and matches what `cagra.rs:751` does for `chunk_count.saturating_mul(dim)`.
- **Suggested fix:** `let ef_search = self.ef_search.max(k.saturating_mul(2)).min(index_size);`. Same one-line change in `cagra.rs:448` (`(k * 2).clamp(itopk_min, itopk_max).max(k)` — `k * 2` here has the same overflow shape).

---

#### AC-V1.38-10: `cmd_index --model X` drift check passes for unknown model even when the preset registry would silently substitute the default
- **Difficulty:** medium
- **Location:** `src/cli/commands/index/build.rs:340-358` + `1497-1525` (`check_index_model_drift`)
- **Description:** Companion to AC-V1.38-7 from the drift-check angle. The drift-check runs *after* `cli.try_model_config()?` — but `try_model_config` reads the resolved `ModelConfig`, which already silently fell back to default for unknown CLI inputs (see AC-V1.38-7). So the drift check has no way to tell the difference between "operator explicitly asked for the same model that's already on disk" and "operator typoed an unknown preset and got the default which happens to match what's on disk". Only the `tracing::warn!` in `ModelConfig::resolve` records the discrepancy, and the drift check doesn't read it. End result: drift check is a sentinel against the dim-mismatch footgun (correct behaviour for that scope), but it's *not* the operator-misconfiguration sentinel its prose suggests. Combined with the asymmetric repo-vs-name match documented in PR #1505's body, the test surface looks comprehensive but only covers the dim-mismatch case.
- **Suggested fix:** Plumb the unknown-preset signal back to the call site: `ModelConfig::resolve` returns `Result<Self, ResolveErr>` (or an additional bool field on the struct, or a separate helper `try_resolve_strict`), and the build path uses the strict variant when it's an explicit `--model` (vs an env/config-file resolution). Pair with AC-V1.38-7's fix so the two findings collapse into a single behavioural change.
