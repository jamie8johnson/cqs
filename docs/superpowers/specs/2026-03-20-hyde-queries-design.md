# SQ-12: Index-Time HyDE Query Predictions

## Purpose

Improve search recall by predicting what queries a developer would type to find each function, embedding those predictions alongside the existing NL description. This closes the gap between how users phrase searches and how functions are described.

Based on doc2query (Nogueira et al.) adapted for code search. Classic HyDE transforms queries at search time (one LLM call per search); we do it at index time (one LLM call per function, amortized) for zero search-time latency.

## Approach

For each callable function, ask the LLM to generate 3-5 short natural language search queries that would match this function. These predictions are appended to the NL description before embedding.

Example for `merge_sort<T: Ord + Clone>(arr: &mut [T])`:
```
stable sort preserving relative order
divide and conquer recursive sort
sort with merge step
generic comparison sort stable
```

The embedding for `merge_sort` now includes both its actual description AND the predicted queries. When a user searches "stable sort preserving order", the embedding distance is much smaller.

## Architecture

### Data Flow

```
cqs index --hyde-queries
  1. Identify callable chunks without existing hyde entry (content_hash + purpose="hyde")
  2. Build batch: prompt with function signature + content (8000 char cap, matching MAX_CONTENT_CHARS)
  3. Submit to Batches API via submit_hyde_batch (purpose = "hyde", max_tokens = 150)
  4. On completion: store predictions in llm_summaries table (purpose = "hyde")
  5. Enrichment pass incorporates hyde predictions into NL → re-embeds affected chunks
  6. Rebuild vector index
```

At search time, nothing changes. The improved embeddings work transparently.

### Index Command Ordering

The hyde pass slots into the existing index command flow:

```
1. Parse + embed chunks (pass 1)
2. --llm-summaries pass (if specified)
3. --hyde-queries pass (if specified) ← NEW
4. Enrichment pass (re-embeds with summary + call context + hyde predictions)
5. Notes indexing
```

**Critical:** Hyde does NOT re-embed on its own. It only generates and stores predictions. The existing enrichment pass (step 4) handles all re-embedding — it already assembles summary + call context, and will be extended to also include hyde predictions. This avoids the enrichment pass overwriting hyde-enriched embeddings.

### Prompt

Single user message (matching existing prompt pattern — no separate system field):

```
You are a code search query predictor. Given a function, output 3-5 short
search queries a developer would type to find this function. One query per
line. No numbering, no explanation. Queries should be natural language,
not code.

Language: {language}
Signature: {signature}

{content, first 8000 chars}
```

**Key decisions:**
- No doc comment in prompt — prevents parroting existing descriptions. We want novel query angles from the code itself. (Note: the doc IS still in the final embedding via `generate_nl_description` — we just don't show it to the LLM for prediction.)
- 3-5 queries — enough diversity, not too noisy.
- Max tokens: 150 — hardcoded in `submit_hyde_batch`, independent of `LlmConfig.max_tokens` (which defaults to 100 for summaries). Same pattern as `submit_doc_batch` which hardcodes 800.
- Content capped at 8000 chars — matches `MAX_CONTENT_CHARS` constant used by summary and doc prompts.

### Storage

Reuses `llm_summaries` table (schema v16, composite PK `(content_hash, purpose)`):

| content_hash | purpose | summary | model |
|---|---|---|---|
| abc123 | summary | "Sorts array using stable merge sort algorithm" | claude-haiku-4-5 |
| abc123 | hyde | "stable sort preserving order\nrecursive merge sort\n..." | claude-haiku-4-5 |

No schema migration needed. The v16 composite PK supports arbitrary purposes.

Cached by content_hash — if function body doesn't change, predictions persist across reindexes.

### Implementation Changes

**`src/llm.rs`:**
- Add `build_hyde_prompt(language, signature, content) -> String` — mirrors `build_prompt` / `build_doc_prompt`
- Add `submit_hyde_batch(&self, store, chunks, max_items) -> Result<String>` — mirrors `submit_doc_batch`. Calls `build_hyde_prompt`, uses `max_tokens = 150`, purpose = `"hyde"`. Cannot reuse `submit_batch` because it hardcodes `build_prompt`.
- Generalize `resume_or_fetch_batch` to accept a `purpose: &str` parameter instead of hardcoding `"summary"`. Or add `resume_or_fetch_hyde_batch` (less clean but follows existing `submit_doc_batch` precedent).
- Add `hyde_query_pass(store, quiet, config) -> Result<usize>` — top-level pass function, mirrors `llm_summary_pass` and `doc_comment_pass`.

**`src/store/mod.rs`:**
- Add `set_pending_hyde_batch_id` / `get_pending_hyde_batch_id` — mirrors existing pending batch metadata for summary and doc batches.

**`src/nl.rs`:**
- Extend `generate_nl_with_call_context_and_summary` to accept `hyde_predictions: Option<&str>`:
  ```rust
  pub fn generate_nl_with_call_context_and_summary(
      chunk, ctx, callee_doc_freq, max_callers, max_callees,
      summary: Option<&str>,
      hyde: Option<&str>,  // NEW
  ) -> String
  ```
  If hyde is present, append `. Queries: {comma-joined predictions}` to the NL. Newlines in stored predictions are converted to commas.

**`src/cli/enrichment.rs`:**
- Pre-fetch hyde predictions alongside summaries (add `get_all_summaries("hyde")` call).
- Pass hyde predictions to `generate_nl_with_call_context_and_summary` as the new parameter.
- Update `compute_enrichment_hash_with_summary` to include hyde text in the hash input. If hyde predictions change, the enrichment hash changes, triggering re-embedding.

**`src/cli/commands/index.rs`:**
- Add `--hyde-queries` flag (gated behind `#[cfg(feature = "llm-summaries")]` — same feature gate as `--llm-summaries`).
- Call `hyde_query_pass` after `llm_summary_pass`, before enrichment pass.
- Add `--max-hyde N` flag.

### NL Description Assembly

After predictions are stored, the enrichment pass re-embeds chunks with the full NL:

```
{summary (SQ-6)} {base NL with signature (SQ-11)}. {call context}. Queries: {prediction1}, {prediction2}, {prediction3}
```

Predictions are comma-joined and appended last. The summary and base NL carry the primary semantic signal; predictions add supplementary query-matching signal at the end.

Full example:
```
Sorts array using stable merge sort algorithm. mnt c projects cqs eval hard rust.
Sort array using merge sort - stable divide and conquer algorithm. merge sort.
Takes parameters: arr: &mut [T]. Uses: merge, sort, left, right, mid.
Signature: pub fn merge_sort<T: Ord + Clone>(arr: &mut [T]).
Queries: stable sort preserving order, divide and conquer recursive sort, sort with merge step
```

### CLI

- `cqs index --hyde-queries` — run hyde prediction pass
- Gated behind `#[cfg(feature = "llm-summaries")]` (same feature as summaries)
- Requires `ANTHROPIC_API_KEY` (same as `--llm-summaries`)
- `--max-hyde N` — cap number of functions to process (same pattern as `--max-docs`)
- Works with batch resume on interrupt (dedicated `pending_hyde_batch` metadata key)
- Can run independently of `--llm-summaries` — different purpose, different cache
- If both `--llm-summaries` and `--hyde-queries` are specified, summaries run first, then hyde, then enrichment pass incorporates both
- **`cqs watch` interaction:** watch does not auto-run hyde for new chunks. Hyde requires explicit `--hyde-queries` flag, same as `--llm-summaries`. New chunks indexed by watch will lack predictions until the next manual `cqs index --hyde-queries`.

## Cost

- ~same as `--llm-summaries` per function (haiku, 150 max tokens out, ~500 tokens in)
- Batches API 50% discount applies
- For 3000 functions: ~1.5M input tokens + 450K output tokens ≈ $0.15
- One-time cost per function body (cached by content_hash)
- Batch cap: 10,000 items per batch (existing `MAX_BATCH_SIZE` constant applies)

## Eval Plan

Run hard eval (55 queries) and stress eval (143 queries, 4266 chunks) with and without hyde predictions. Measure R@1, MRR, per-language breakdown. Focus on:
- TypeScript MRR (weakest at 0.814 with signatures)
- Rust stress MRR (0.037 with RRF — the hardest case)
- Whether predictions help or hurt confusable function discrimination

## Future: Discriminating Descriptions (Option B)

If query predictions alone aren't sufficient, a second purpose `"discriminate"` could generate "what makes this function unique compared to similar functions." This was considered alongside query predictions but deferred — predictions directly close the query-document gap, while discriminating descriptions address a different problem (confusable function precision). Can be added as a separate pass without changing the architecture.

## Not In Scope

- Query-time HyDE (LLM call per search) — index-time approach eliminates search latency
- Sync Messages API endpoint in llm.rs — not needed since we use Batches API
- Schema migration — v16 composite PK already supports new purposes
- Changes to search pipeline — embeddings work transparently
