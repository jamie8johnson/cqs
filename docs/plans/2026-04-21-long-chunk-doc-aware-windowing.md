# Long-Chunk Doc-Aware Windowing

**Status:** Tested → NEUTRAL 2026-04-21 (within HNSW reconstruction noise; no measurable effect). See "Empirical result" + "Meta-finding" below.
**Author:** opus 4.7 + jjohnson
**Date:** 2026-04-21

## Empirical result (post-implementation A/B)

Implementation simplified the spec: instead of `MAX_PREAMBLE_TOKENS = 128` truncation, the change just preserved `chunk.doc.clone()` on every window in `apply_windowing()`. PARSER_VERSION bumped 1 → 2 to force re-embed of all windowed chunks. Reindex (~50 min including HNSW rebuild — daemon held shared lock on HNSW, had to stop daemon for write lock).

**Three-cell A/B on v4 (n=1526 per split), all on the same code at different times:**

| Cell | When | test R@5 | dev R@5 |
|---|---|---|---|
| PR #1069 baseline | original v4 measurement | 48.9% | 49.9% |
| Lever ON (this change) | post-fix reindex | 45.0% | 46.1% |
| Lever OFF (revert) | reindex back to baseline | 44.8% | 46.0% |

The decisive comparison is **lever-ON vs lever-OFF on adjacent reindexes (same HNSW realization basin):** 45.0 vs 44.8 on test (+0.2), 46.1 vs 46.0 on dev (+0.1). **Within noise.** Per-category deltas all ±1.6pp at n=200 — also within noise.

The 3.9pp gap to PR #1069's baseline is **not** caused by the lever. It's an artifact of HNSW reconstruction varying between reindexes.

## Meta-finding: HNSW reconstruction noise is ~4pp R@5 at v4 N

Two reindexes of the same source corpus (no code change between them) produced ~4pp R@5 swing on v4 (1526 queries per split). HNSW graph construction is order-dependent — the insertion order of chunks affects neighbor selection, which affects retrieval at the recall boundary.

**Implications:**

1. **Our entire R@K measurement history sits in this noise floor.** Tier 3's claimed +5.5pp test R@5 at v3 n=109 (~12pp noise-equivalent on v3's smaller N) may have been partly noise-driven, partly real. The chunker doc-fallback for short chunks is plausibly real (the mechanism is concrete and the per-category structural signal was directional), but the magnitude is uncertain.
2. **Small-N A/Bs (v3 at n=109) are unreliable for ±5pp claims.** Even at v4 n=1526 the noise floor is ±4pp.
3. **Future A/B protocol must isolate HNSW reconstruction.** Two options:
   - **Fixed-seed HNSW construction** — feed `hnsw_rs` or CAGRA a deterministic insertion order keyed on chunk_id. Removes the noise entirely. Requires upstream changes to the HNSW build path.
   - **Paired-reindex baselines** — for any A/B requiring a reindex (chunker changes, embedder changes, content_hash-affecting changes), measure baseline AND lever on adjacent reindexes; only compare the pair. Bumps reindex cost 2x but isolates signal.
   The paired-reindex approach is the cheaper near-term fix. Adopt as default for any future windowing/chunking/embedding A/B.
4. **The Tier 3 → v1.28.x platform numbers should be re-validated under paired reindex** before being cited as production-quality wins. Specifically: rerun Tier 3 short-chunk doc-fallback A/B with paired-reindex baselines on v4. If the +5.5pp survives, the lever is real. If it shrinks to ±2pp, we lost less than we thought when shipping Tier 3 (it was the right call regardless given the per-category structural signal, but the magnitude was inflated by noise).

**Lever-specific verdict for THIS spec:** doc-on-every-window is **inert** at v4 N — neither helps nor hurts within the noise floor. The hypothesized "embedding dilution" theory was post-hoc rationalization of noise; the cleaner reread is "the lever has no measurable effect."

**Reverted in same session.** PARSER_VERSION reset to 1, source reverted to main. Final reindex restored baseline.

**What might still work for the same underlying goal** (deferred):
1. **Per-window summarization via LLM** — each window gets a unique short summary describing its specific content, adding per-window discrimination rather than uniform doc anchoring. Multi-week build (LLM cost per chunk per reindex).
2. **Multi-granularity index** — function-level (with doc) AND paragraph-level (without doc). Spec'd in "Future work" below.
3. **Cross-window contrastive embeddings** — learned aggregator that combines window embeddings while preserving discrimination. Research-grade.

These remain plausible. This specific lever (uniform doc preamble per window) is closed.

---

## Original spec (preserved below for reference)


## Problem

The Tier 3 chunker doc-fallback (PR #1040 + #1041 hardening, ~v1.27.x → v1.28.0) shipped the largest single retrieval win this project has measured: **+5.5pp R@5 test, +3.7pp R@5 dev** on `v3.v2` over canonical baselines. The mechanism: for chunks shorter than `SHORT_CHUNK_LINE_THRESHOLD = 5` source lines, `extract_doc_fallback_for_short_chunk()` walks back up to `FALLBACK_DOC_MAX_LINES = 8` preceding lines of comments and attaches them to the chunk's `doc` field. `generate_nl_description()` then composes the embedding text from `signature + doc + content`, so the embedding captures the developer's stated intent rather than just the (often trivially short) source body.

Tier 3 only addresses **short** chunks. The exactly-symmetric problem on the **long-chunk** side remains unaddressed:

When a chunk's text exceeds the embedder's token budget (BGE-large: 512 tokens, ~2 KB of source), `Embedder::split_into_windows()` slices it into overlapping windows. Each window is independently embedded and stored as its own row in the `chunks` table (with the same parent `chunk_id` lineage but distinct `window_index`). **Only window 0 contains the function's doc comment + signature.** Windows 1, 2, … N each contain a raw source slice from the middle or tail of the function body, with no preamble identifying what function they belong to or what the function does.

This produces a class of failure modes:

1. **Conceptual / behavioral queries miss long functions.** A query like "validates user input" should match a function whose doc says "Validates the incoming request body" — but if that validation logic is split across 3 windows, only window 0 has the doc text. Windows 1 and 2 are bare loop bodies, branch logic, error handling: their embeddings represent the *mechanism*, not the *purpose*.
2. **Identifier_lookup queries miss long functions when the matched code is past window 0.** If a function has doc + signature in window 0 and the user's grep'd identifier appears in window 2, BGE retrieves window 2 by lexical match but the query's category (`identifier_lookup`) routes via SPLADE-heavy α=1.0, where window 2's lack of named identifier in its embedding becomes a problem.
3. **Reranker pool dilution.** When window 0 IS retrieved, the reranker sees `signature + doc + window_0_content`. When window 2 is retrieved, the reranker sees only `window_2_content`. Same function, two very different reranker contexts. Reranker's relevance score loses calibration.

The pipeline already pays the cost of splitting long chunks into multiple rows. It pays the cost of running BGE inference per window. It does NOT pay the (very small) cost of attaching the parent function's doc + signature to each window's embedding text.

## Goal

Extend Tier 3's "embed the developer's stated intent, not just the body bytes" principle to long chunks. Every window of a windowed chunk should carry the parent function's doc + signature into its embedding text.

**Success criterion:** test R@5 ≥ baseline + 1.5pp on `v4_test.v2.json` (n=1526), with no R@5 regression on `v4_dev.v2.json` (n=1526), AND the per-chunk-length subset (queries whose gold is a windowed chunk, ~10-15% of corpus) shows ≥ +3pp R@5 lift in isolation.

The dual gate (overall + windowed-subset) is deliberate: the overall lift is the user-facing metric, but the windowed subset is the mechanism we're trying to fix. If the overall metric moves but the windowed subset doesn't, we've confused ourselves and should reject the result. If the windowed subset moves but overall doesn't, the lever works but its share of the corpus is too small to matter at the headline metric — fine to ship anyway, it's still strictly better.

## Non-goals

- **Multi-granularity index.** Indexing the same function at function-level + paragraph-level + file-level is a separate larger build (spec'd as future work below). This proposal does not change the granularity, only what each existing window's embedding text contains.
- **Semantic boundary chunking.** Replacing token-stride windowing with semantic-boundary windowing (split at logical block boundaries instead of token offsets) is also separate. This proposal keeps the existing token-stride windowing.
- **Per-language chunk strategy.** Today all 54 languages share the same windowing path. Per-language differentiation (Rust impl blocks vs Python classes vs SQL queries) is future work.
- **Cross-window embedding fusion.** Combining the N window embeddings into a single parent-function embedding (mean-pool, max-pool, learned aggregator) is a different design with different schema impact.
- **Schema changes.** No new SQLite columns, no new table, no migration. The `doc` and `signature` fields already exist on the chunk row; we just need to compose them into the per-window embedding text.

## Decision summary

Modify `Embedder::split_into_windows()` (or its caller in the index pipeline) so that for each window past index 0, the embedding text is composed as `[parent.doc] + [parent.signature] + [window_content]` instead of just `[window_content]`. Window 0 keeps its current behavior (`generate_nl_description()` already composes doc + signature + content for it, then `split_into_windows()` slices that composed text — so window 0 already has the doc + signature naturally; the fix is only for windows 1+).

The composition steals tokens from the window's content budget — if the doc + signature take 100 tokens, the window content budget drops from 512 to ~412. This is a deliberate tradeoff. The current windowed-chunk problem isn't that we're losing 100 tokens of body content per window — it's that we're losing 100% of the doc context. Trading 100 tokens of body for 100 tokens of doc is approximately net positive on the "is this window semantically tagged" axis.

Index-time only. No router changes. No call-site changes. No `Cargo.toml` version bump until the A/B confirms.

## Background

### Tier 3 recap

PR #1040 (`fix(parser): doc enrichment for short chunks`) and the follow-on hardening in #1041 introduced `extract_doc_fallback_for_short_chunk()` at `src/parser/chunk.rs:393`. The function:

1. Gates on `line_end - line_start < SHORT_CHUNK_LINE_THRESHOLD` (5 lines).
2. Walks back from the chunk's `start_byte` through the source prefix, collecting up to `FALLBACK_DOC_MAX_LINES = 8` lines that are comment-like for the chunk's language.
3. Caps consecutive blank lines at `MAX_CONSECUTIVE_BLANKS = 16` to defend against pathological files.
4. Returns the captured comment text, which the caller assigns to the chunk's `doc` field.

`generate_nl_description()` at `src/nl/mod.rs:189` then composes the chunk's embedding text from name, signature, doc (if present), parent context, file context, and content. The embedding is the BGE-large output on this composed text.

The measured effect (post-#1040 baselines vs canonical pre-#1040, fresh-fixture comparison from PROJECT_CONTINUITY's lever-by-lever table):

| Metric | Canonical | post-#1040 | Δ |
|---|---|---|---|
| test R@5 | 63.3% | 67.0% | +3.7pp |
| test R@20 | 80.7% | 75.2% | -5.5pp (later stabilized post-reindex to +4.6pp current vs canonical) |
| dev R@5 | 74.3% | 71.6% | -2.7pp (later +3.7pp post-stabilization) |
| dev R@20 | 86.2% | 79.8% | -6.4pp (later +1.9pp post-stabilization) |

The deeper analysis in PROJECT_CONTINUITY's "v3 baselines" section confirms that after corpus pruning stabilized (chunk count 14,734 → 16,150 across reindexes), all metrics ended above canonical. Tier 3 + the windowing fix + classifier flip cumulatively delivered the v1.28.x R@5 platform.

The pattern: when the embedding text describes WHAT the chunk does (via doc) in addition to HOW (via content), retrieval improves disproportionately on queries that ask about behavior or intent. This pattern is the foundation of this proposal — long chunks have the same problem (their windows lack "what does this do" context) but Tier 3's gate excluded them.

### Windowing recap

`Embedder::split_into_windows()` at `src/embedder/mod.rs:522` produces overlapping windows when the embedding text exceeds `max_tokens`. The defaults (per the embedder config used in production):

- `max_tokens = 512` (BGE-large context window)
- `overlap = 64` (token overlap between consecutive windows)
- Step = `max_tokens - overlap = 448` tokens per window

The windowing fix in v1.28.2 (PR #1060) corrected a lossy WordPiece decode that was storing space-separated subword text as `chunks.content` (e.g. `"pub fn save ( & self , path : & path )"`). The fix uses `encoding.get_offsets()` to slice the original source by character offsets, preserving exact bytes for cross-encoder reranking, result display, and NL summary generation.

What the fix did NOT change: the **per-window embedding text is still just the window's source slice.** Windows past index 0 do not see the function's doc, signature, or any parent context. Each window's embedding represents only its own slice's surface content.

Production stats (from `cqs stats`): of 16,150 chunks indexed today, approximately 10-15% are windowed (>1 window per chunk). The exact number is recoverable from the SQLite `chunks` table by `SELECT COUNT(*) WHERE window_index > 0`. This is the population this proposal targets — the worst window-loss case is on long Rust functions in `src/store/`, `src/embedder/mod.rs`, and `src/parser/`.

### Empirical signal

From the Phase 1.4b A/B per-category results on v3 (n=109 per split), the categories most likely to benefit:

- **conceptual_search**: 38.5% R@5 baseline on v3 test, 41.7% on v3 dev. Worst category. Conceptual queries should match doc text > body text; today they only match doc text on window 0.
- **behavioral_search**: 75.0% / 87.5%. Mid-tier. Behavioral queries match action verbs in doc > control flow in body.
- **type_filtered**: 69.2% / 69.2%. Mid-tier. Less clear effect — type filtering is more about chunk_type than doc text.

Categories likely UNAFFECTED:
- **identifier_lookup**: 94.4% / 100.0%. Best category. The identifier is in `name`/`signature`, not in window content. SPLADE-heavy routing (α=1.0) finds it via lexical match in window 0 anyway. Long-chunk extension shouldn't hurt or help much.
- **negation**: 87.5% / 94.1%. Strong. Negation depends on the "without X" or "not Y" phrasing in the query matching the doc, again on window 0.

The hypothesis is testable per-category: conceptual + behavioral should show the largest lift; identifier_lookup + negation should be approximately flat. If we see the opposite — identifier_lookup lifts but conceptual flat — then either (a) the doc preamble is acting as a generic-language anchor (unlikely on this corpus state) or (b) we measured the wrong thing (more likely).

## Hypothesis

**H1 (primary):** Adding the parent chunk's doc + signature as a preamble to every windowed chunk's embedding text will lift R@5 on conceptual + behavioral query categories by ≥ 5pp on the windowed-chunk subset of v4, with overall R@5 lift of ≥ 1.5pp on test (no dev regression).

**H2 (alternative — null):** R@5 is insensitive to per-window doc preamble at the v4 noise floor (~±0.6pp per-category at n=180, ~±0.3pp overall at n=1526). The lever is exhausted because long-chunk recall is already good enough via window 0 + reranker repair.

**H3 (alternative — backfire):** Stealing tokens from the window content budget hurts identifier_lookup more than the doc preamble helps conceptual + behavioral. Net R@5 regression overall.

The decision matrix (in Validation below) maps each outcome to a concrete next move.

## Design

### Architecture

The change lives at the seam between `generate_nl_description()` and `split_into_windows()`. Today the data flow is:

```
Chunk { name, signature, doc, content, ... }
     ↓
generate_nl_description(chunk) → embedding_text   [composed: signature + doc + content]
     ↓
split_into_windows(embedding_text, max_tokens=512, overlap=64) → Vec<(window_text, window_idx)>
     ↓
For each (window_text, window_idx):
    BGE.embed_documents(window_text) → window_embedding
    INSERT INTO chunks (chunk_id, window_index, embedding, content) VALUES (...)
```

The proposed flow:

```
Chunk { name, signature, doc, content, ... }
     ↓
preamble = compose_window_preamble(chunk)      [signature + doc, no content]
content_text = chunk.content                   [body only]
     ↓
content_windows = split_into_windows(content_text, max_tokens=512 - preamble_tokens, overlap=64)
     ↓
For each (content_window, window_idx):
    embed_text = preamble + content_window     [every window gets the preamble]
    BGE.embed_documents(embed_text) → window_embedding
    INSERT INTO chunks (...)
```

Two changes:

1. **Separate composition from windowing.** `generate_nl_description()` currently produces the entire embedding text in one shot. We need to split it into `preamble` (always-prepend) and `content_text` (windowable). Window 0 ends up with the same total text as today (since `preamble + first_window_of_content == today's window_0_text`).
2. **Reduce content window budget by preamble size.** If the preamble takes 100 tokens, the content window can hold 412 tokens. The total per-window embedding text stays at 512.

### Per-window text composition

The preamble is fixed across windows for a given chunk. It contains:

```
{signature}\n{doc}\n
```

For Rust:
```
fn validate(&self) -> Result<(), Error>
/// Validates the incoming configuration against the schema.
/// Returns Err if any required field is missing or has an invalid type.
```

For Python:
```
def validate(self) -> bool:
"""Validate incoming configuration. Returns True if valid."""
```

The exact composition (signature first vs doc first vs both interleaved) should follow whatever `generate_nl_description()` does today, just truncated to omit `content`. Pulling the preamble out of `generate_nl_description()` requires either:

- **Option A:** add a flag `generate_nl_description(chunk, exclude_content: bool)` that returns the composed text minus the body, OR
- **Option B:** add a separate `compose_chunk_preamble(chunk) -> String` helper that mirrors the `generate_nl_description()` ordering for everything except the body.

Option B is cleaner — `generate_nl_description()` is already complex with its template variants and section-chunk specialization; adding a flag risks branching the body logic. A separate helper keeps the preamble logic readable and testable in isolation. The cost is one duplication of the field-ordering decision; mitigated by a unit test that asserts `compose_chunk_preamble(c) + c.content == generate_nl_description(c)` for non-windowed chunks (so the two paths cannot drift).

### Doc detection (which comments count)

For the preamble, we use the chunk's existing `doc` field — populated by either:

- The tree-sitter parser's @doc capture (the standard path for languages with first-class doc syntax: Rust `///`, Python `"""..."""` docstrings, JSDoc `/** */`)
- `extract_doc_fallback_for_short_chunk()` for chunks ≤ 5 lines without a parsed doc

This proposal does NOT introduce a new doc-detection path. If the chunk has no doc, the preamble is just the signature. If the chunk has no signature either (rare — usually section chunks or anonymous closures), the preamble is empty and the windowing falls back to today's behavior.

**Open question:** should we ALSO run a long-chunk version of `extract_doc_fallback_for_short_chunk()` for chunks that lack a parsed doc but have leading comments? The current Tier 3 logic gates on `line_end - line_start < 5` — so a 50-line function with a leading comment block but no `///` doc syntax (e.g., a SQL stored procedure with `--` header comments) gets no doc. This is a real gap. Spec'd as open question #3 below.

### Token budget allocation

The key tradeoff: how many tokens does the preamble get?

- **Too few tokens** → preamble truncated, lose the doc text past the cutoff
- **Too many tokens** → content window shrinks, more windows per long chunk, more rows in the index

Empirical sizing on cqs's corpus:
- Median Rust function signature: ~15 tokens
- Median Rust function `///` doc: ~40 tokens (1-3 lines)
- Long doc outliers (top 10%): ~150 tokens (extensive doc-comment blocks like `src/parser/chunk.rs::extract_doc_fallback_for_short_chunk`)

A `MAX_PREAMBLE_TOKENS = 128` constant captures the median case + most outliers, leaves 384 tokens for content (75% of original 512). Long doc outliers get truncated at 128 tokens, which is acceptable — the first 128 tokens of any doc capture the "what does this do" semantic anchor.

Default constant: `MAX_PREAMBLE_TOKENS = 128`. Override via env: `CQS_LONG_CHUNK_PREAMBLE_TOKENS`. Sweepable via the ablations table below.

### Schema impact

None. The `chunks` table already has `doc` and `signature` columns. The window's `content` column continues to store the raw source slice (no preamble baked in), so reranker / display / NL generation paths still see the exact source. The preamble only affects the `embedding` column's value (since BGE was given preamble + content_window as input).

Reranker note: at scoring time, the cross-encoder reranker takes (query, chunk_content) pairs. The reranker does NOT see the preamble. This is a minor calibration mismatch — the embedding ranks the chunk via preamble + content, but the reranker scores it via content alone. This mismatch already exists today for non-windowed chunks (embedding takes signature + doc + content, reranker takes content only); we're not introducing a new issue, just propagating the existing one to long chunks.

If the calibration mismatch becomes a measured problem in the A/B, the followup is straightforward: feed the reranker `signature + doc + content` instead of `content` alone. That's a separate spec.

### Reindex strategy

Full reindex required. The `embedding` column for every windowed chunk must be recomputed — there is no incremental path because we're changing what BGE sees.

Cost: ~30 minutes on RTX 4000 for the cqs corpus (16,150 chunks, ~15% windowed = ~2,400 chunks needing recompute, but the daemon re-embeds everything that doesn't match the cached `content_hash` — and we're changing the embedding inputs, so all windowed chunks get recomputed regardless of content hash since they don't match the on-disk hash).

The daemon-restart pattern from the v1.28.2 invalidation hook applies here too: `cqs index --force` rebuilds, then the daemon needs to restart to pick up the new HNSW. Standard ops.

**Cost optimization (optional, deferred):** the `chunks` table could carry a `preamble_hash` column that gets bumped when the preamble logic changes. Then incremental reindexes can detect "preamble changed, must re-embed" without forcing a full rebuild. Not worth building until the lever ships green and we're iterating on doc detection variants.

### Code paths affected

```
src/parser/chunk.rs
    + compose_chunk_preamble(chunk: &Chunk) -> String
    + unit test: preamble + content == generate_nl_description(chunk) for non-windowed

src/nl/mod.rs
    + (no changes to generate_nl_description; the helper lives in chunk.rs and
       composes the same fields)

src/embedder/mod.rs
    M Embedder::embed_documents — for chunks needing windowing, slice the
       content separately and prepend the preamble per window
    M split_into_windows — accepts an optional `preamble: Option<&str>` and
       a `max_preamble_tokens: usize` for budget allocation
    + Constants: DEFAULT_LONG_CHUNK_PREAMBLE_TOKENS = 128
    + Env override: CQS_LONG_CHUNK_PREAMBLE_TOKENS

src/cli/commands/index/build.rs
    + (no direct changes; the change is internal to embedder)

evals/long_chunk_doc_aware_ab.py
    + new — A/B harness specific to this lever, with windowed-chunk subset filter

tests/integration/embedder_test.rs (or wherever the windowing fix's regression
   test lives)
    + assert that windows 1+ contain the chunk's doc text
```

## Index-time pipeline diagram

```
file: src/parser/chunk.rs (long function with /// doc)
                ↓
        Parser produces Chunk:
                name = "extract_doc_fallback_for_short_chunk"
                signature = "fn extract_doc_fallback_for_short_chunk(node, source, ...)"
                doc = "/// Extract a doc-comment fallback for chunks too short to..."
                content = "    if line_end.saturating_sub(line_start) >= SHORT_CHUNK_LINE_THRESHOLD {\n        return None;\n    }\n    ..."
                ↓
        compose_chunk_preamble(chunk) → preamble (≤ 128 tokens)
                "fn extract_doc_fallback_for_short_chunk(node, source, ...)
                 /// Extract a doc-comment fallback for chunks too short to..."
                ↓
        split_into_windows(content, max_tokens=512-128=384, overlap=64) → 3 windows
                window_0: content[0..380]
                window_1: content[316..696]
                window_2: content[632..1012]
                ↓
        For each window:
            embed_text = preamble + "\n" + window_content
            BGE.embed_documents(embed_text) → embedding
            INSERT chunks (chunk_id, window_index, embedding, content=window_content, ...)
                ↓
        Three rows in chunks table:
                (chunk_id=X, window_index=0, embedding=E0, content="    if line_end...")
                (chunk_id=X, window_index=1, embedding=E1, content="    ...")
                (chunk_id=X, window_index=2, embedding=E2, content="    ...")
        Each E_i is BGE(preamble + window_content), not BGE(window_content alone)
```

## Validation

### A/B design

Three cells on `v4_test.v2.json` and `v4_dev.v2.json` (both at n=1526, the proper-N fixture from PR #1069). Plus a windowed-chunk subset filter for the per-mechanism check.

**Cell 1 (baseline):** v1.28.3 production. Tier 3 short-chunk doc fallback ON (already shipped). No long-chunk doc preamble. Standard windowing.

**Cell 2 (this lever):** v1.28.3 + long-chunk doc preamble at `MAX_PREAMBLE_TOKENS = 128`. Full reindex required. Tier 3 short-chunk fallback continues to apply (the two paths don't overlap by construction — short chunks aren't windowed).

**Cell 3 (sweep ceiling):** Cell 2 with `MAX_PREAMBLE_TOKENS` swept across {32, 64, 128, 256}. Pick the best for the headline number.

The sweep is a separate phase after the binary A/B confirms the lever direction. If Cell 2 vs Cell 1 shows the wrong sign, don't bother sweeping.

### Subset filter

A new helper in the eval harness selects only queries whose `gold_chunk` is a windowed chunk:

```python
# evals/long_chunk_doc_aware_ab.py
def is_windowed_gold(gold_chunk_id, store_db):
    cur = store_db.execute(
        "SELECT MAX(window_index) FROM chunks WHERE chunk_id = ?",
        (gold_chunk_id,)
    )
    return cur.fetchone()[0] > 0
```

The subset is the population this lever targets. Reporting it separately catches the case where overall R@5 moves but the mechanism didn't fire (would indicate a confound somewhere else).

### Decision matrix

| Result | Action |
|---|---|
| Overall R@5 ≥ baseline + 1.5pp on v4 test, no dev regression, AND windowed subset ≥ +3pp R@5 | Ship as v1.29.0. Set `MAX_PREAMBLE_TOKENS = 128` default. |
| Overall R@5 between baseline and +1.5pp, windowed subset ≥ +3pp | Ship behind opt-in env (`CQS_LONG_CHUNK_DOC_PREAMBLE=1`). Default off until corpus shape changes enough to surface the lever at the headline metric. |
| Windowed subset moves but overall doesn't | Ship behind opt-in. The mechanism is right; the corpus's windowed-chunk share is too small to dominate. |
| Overall R@5 ≥ baseline + 1.5pp BUT windowed subset flat | Investigate. Lever fired in the wrong place. Likely a confound (e.g., the reindex itself moved the metric, not the doc preamble). Re-run with the v1.28.3 binary on a freshly-reindexed v3 to confirm baseline didn't drift. |
| Cell 2 vs Cell 1 R@5 regression | Inspect per-category. If conceptual + behavioral lift but identifier_lookup tanks, run sweep with smaller `MAX_PREAMBLE_TOKENS` to free more content tokens. If everything regresses, the per-window preamble is acting as embedding noise — kill the lever. |
| Per-category breakdown shows conceptual lift > behavioral lift | Hypothesis confirmed direction. Proceed to sweep. |
| Per-category shows identifier_lookup lift > conceptual lift | Hypothesis backwards. Investigate — possible the preamble is helping lexical matches rather than semantic matches. Could indicate the right lever is "richer chunk-content text" not "doc preamble per window". |

### Per-chunk-length breakdown

Beyond per-category, also report by parent-chunk window count:

| Parent windows | R@5 baseline | R@5 cell 2 | Δ |
|---|---|---|---|
| 1 (non-windowed) | — | — | should be ±0 (the lever doesn't apply) |
| 2-3 windows | — | — | expected lift |
| 4+ windows | — | — | expected larger lift (more windows that today lack doc) |

If the 1-window subset moves at all, it means the harness or reindex introduced a confound. That row is the noise floor; everything else is signal.

## Open questions

1. **Token budget tradeoff.** `MAX_PREAMBLE_TOKENS = 128` is a guess from corpus medians. The right value is empirical — sweep at {32, 64, 128, 256} on cell 2 before deciding the default. Consider per-language defaults if the sweep shows divergent optima (Rust `///` is short, SQL `--` headers are long).

2. **Doc choice when multiple comments exist.** If a function is preceded by `// region: parser internals` block-comment + `/// Extract a doc-comment fallback` line-comment + a blank + the function, which counts as the doc? Today's parser captures the `///` block (the immediately-preceding comment-like construct). The fallback for short chunks walks back further. For the long-chunk preamble we should match today's parser behavior — only the immediately-preceding `///` block — because adding the region comment as preamble noise probably hurts more than it helps. Leave the multi-comment composition to a later iteration if specific evidence shows it matters.

3. **Doc fallback for long chunks without parsed docs.** A 50-line SQL stored procedure with `-- header description` but no parsed doc currently has `chunk.doc = None`. This proposal would emit only the signature as preamble for such chunks. A "long-chunk doc fallback" — analogous to Tier 3 but without the line-count gate — would extract the leading comments for any chunk lacking a parsed doc. Spec'd as a follow-on if the binary A/B for the basic preamble path lands green; otherwise irrelevant.

4. **Preamble token cost calibration with CQS_MAX_SEQ_LENGTH override.** The pipeline supports embedders with longer context windows via `CQS_MAX_SEQ_LENGTH`. If a future deployment uses a 2048-token embedder, the preamble budget should scale proportionally. Default: `MAX_PREAMBLE_TOKENS = max_seq_length / 4`. Cap: 256 (no value in flooding the embedding with doc text past that point).

5. **Interaction with the existing Tier 3 short-chunk path.** Tier 3 fills `chunk.doc` for chunks ≤ 5 lines. Short chunks are never windowed (5 lines fits in 512 tokens easily). So the two paths should not overlap. But the gate is on **source lines**, not **token count** — a short chunk with very long lines (e.g., a 5-line function with a 200-character signature on one line) could in principle exceed 512 tokens. Spec'd as a corner case to validate via `cargo test parser::chunk::tests::short_chunk_does_not_window`.

6. **Reranker calibration mismatch.** The reranker scores `(query, content)` not `(query, preamble + content)`. This proposal worsens the existing mismatch slightly (more rows now have embeddings that include the preamble while the reranker scores content alone). Acceptable for v1; if A/B shows reranker score variance increased, the followup is feeding `signature + doc + content` to the reranker too. Separate spec.

7. **Window 0 double-prepending.** Window 0's content already starts with `signature + doc + ...` because `generate_nl_description()` composes those at the front of the embedding text. If we now ALSO prepend `preamble = signature + doc` to window 0's content, the embedding sees `signature + doc + signature + doc + body[0..N]`. Non-fatal but wasteful. Mitigation: detect window 0 and skip the preamble for it (window 0 ALREADY has the preamble naturally). Implementation: `if window_index > 0 { prepend preamble }`. Test: `assert!(window_text(0).starts_with(signature))`.

8. **Cache invalidation for incremental reindex.** The `content_hash` SQLite column today catches "content changed → re-embed needed." This proposal doesn't change content; it changes the EMBEDDING INPUT for windowed chunks. So `content_hash` won't trigger re-embedding when we ship this lever — incremental reindex would silently keep the old embeddings. Two fixes: (a) full `cqs index --force` on first deployment, (b) bump a new `parser_version` (already exists per audit P2 #29 — used for parser logic changes) to force re-embed. Use (b); it's the existing pattern for index-affecting parser-side changes.

## Ablations

Sweep alongside the binary A/B. Each cell runs once on v4 test+dev with the rest of the knobs at defaults.

| Knob | MVP | Sweep range | Hypothesis being tested |
|---|---|---|---|
| `MAX_PREAMBLE_TOKENS` | 128 | {32, 64, 128, 256, 384} | Diminishing returns past 128; backfire past 256 |
| Preamble composition | `signature + doc` | {`signature only`, `doc only`, `signature + doc`, `name + signature + doc`} | Doc-only might be cleanest; name might add SPLADE lexical anchor |
| Window overlap | 64 (current) | {32, 64, 128} | Larger overlap = more windows = doc-prepended in more places, but redundant |
| Apply to window 0 | False (skip) | {True, False} | Confirms the wasteful double-prepend hurts measurably |
| Per-language preamble length | Uniform 128 | Rust=128, Python=64, SQL=192 | Rust `///` runs short; SQL `--` runs long. Uniform may waste budget |
| `MAX_CONSECUTIVE_BLANKS` for fallback (if open Q #3 added) | 16 (current) | {0, 16, 32} | Tighter blank tolerance = stricter doc detection |

Report each as R@5 delta vs MVP. Pick the per-knob winner before final A/B.

## Alternatives considered

**Multi-granularity index (function + paragraph + file):** Indexes the same code at multiple granularities, picks the best per query. Theoretically the right answer — different queries want different scopes. But it triples the index size, complicates the search path, and requires a per-query granularity selector (which is itself a routing problem we just spent a session learning is hard on this corpus). Long-chunk doc-aware windowing gets ~30% of the multi-granularity benefit at ~5% of the build cost. Spec'd as future work; revisit if this lever ships green and we want more.

**Semantic boundary chunking:** Replace token-stride windowing with windowing at logical block boundaries (loop bodies, conditional branches, top-level statements). Likely better than token-stride because each window is semantically coherent. But requires a tree-sitter pass on the chunk content, language-specific block-detection logic, and re-architecting the windowing API. Long-chunk doc preamble is much simpler and probably gets most of the same lift on the conceptual + behavioral query categories.

**Mean-pool window embeddings into a parent embedding:** Compute one embedding per parent function as the mean of its window embeddings. Indexing 1 row per function instead of N rows. Smaller index, simpler retrieval. But mean-pooling washes out per-window semantic detail — long functions become bag-of-words representations. Loses the existing benefit of per-window granularity (a query that matches a specific loop in a long function wouldn't find it via the parent embedding). Wrong tradeoff for this corpus.

**Reranker repair:** Feed the reranker `signature + doc + content` instead of `content` alone. Closes the embedding/reranker calibration mismatch. Doesn't help embedding-side recall (which is what this proposal targets). Useful as a follow-on but doesn't substitute.

**Cross-window context smoothing:** Each window includes N tokens of the previous window's tail and N tokens of the next window's head. The current overlap=64 already does some of this, but pure cross-window smoothing could go further (e.g., overlap=256). Increases windows-per-chunk and doesn't address the core gap (windows lack doc context, not body context). Wrong axis.

**Train chunk-to-chunk-text mapping via a learned encoder:** Learn a function (Chunk struct → embedding text) that maximizes downstream R@5. Most general but biggest build. Defer to a research arc once the deterministic improvements are exhausted.

## Future work

**Multi-granularity index.** Index every code unit at three granularities: function-level (today's behavior), paragraph-level (sub-units of long functions split at semantic boundaries), file-level (one row per file with a top-level summary). At search time, retrieve top-K from each granularity, fuse via RRF or weighted score, return the best per query. Requires schema extension for granularity tag, retrieval-side fusion, and storage cost ~3x. Spec separately when this proposal ships green.

**Per-language chunk strategy.** Today the windowing path is uniform across 54 languages. But Rust `impl` blocks, Python class bodies, SQL stored procedures, and L5X/L5K PLC programs have different ideal chunk shapes. A per-language `ChunkingPolicy` trait could specify (max_chunk_lines, signature_extraction, doc_detection, preamble_composition) with sensible defaults. Significant refactor; defer until at least one language shows a clear win where the uniform strategy is the bottleneck.

**Embedding-aware semantic chunking.** Chunk by token budget at the parser level — never produce a chunk that exceeds the embedder's context window in the first place. Eliminates windowing entirely. Trades parser complexity for embedder simplicity. Could lift R@5 by ensuring every chunk's embedding represents a complete coherent unit. Requires per-language understanding of "what's a coherent splittable unit at this token budget." Multi-week build.

**Reranker preamble parity.** Spec the reranker calibration fix referenced in open question #6 — feed the cross-encoder `signature + doc + content` instead of `content` alone. Small change, closes a mismatch that's been there since the reranker shipped.

**Prompt-aware preamble.** Vary the preamble per query category — for behavioral queries, include extra verb-rich text; for type_filtered queries, include the chunk_type and parent_type. Requires per-query preamble computation at retrieval time, which doesn't match the current architecture (preambles are baked into embeddings at index time). Would need a retrieval-time re-embedding path. Probably not worth it; the per-window doc preamble captures most of the same signal at index-time cost.

**Hierarchical embeddings.** A long function gets one parent embedding (computed from preamble + condensed summary) and N window embeddings (per-window slices). Search returns the parent if the query is conceptual; returns a window if the query is structural. Requires a query-side decision (which embedding to compare against), bringing back the routing problem we parked. Defer until routing has a fresh idea behind it.
