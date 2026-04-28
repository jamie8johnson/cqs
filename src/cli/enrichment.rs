//! Second-pass enrichment: re-embed chunks with call graph context.
//!
//! After the main pipeline populates the `function_calls` table, this pass:
//! 1. Computes callee document frequency (IDF) for stopword filtering
//! 2. Iterates all chunks in pages
//! 3. For each chunk with callers or callees, regenerates NL with call context
//! 4. Re-embeds and updates the embedding in-place

use std::collections::HashMap;

use anyhow::{Context, Result};
use indicatif::{ProgressBar, ProgressStyle};

use cqs::{Embedder, Embedding, Store};

/// Second-pass enrichment: re-embed chunks with call graph context.
/// After the main pipeline populates the `function_calls` table, this pass:
/// 1. Computes callee document frequency (IDF) for stopword filtering
/// 2. Iterates all chunks in pages
/// 3. For each chunk with callers or callees, regenerates NL with call context
/// 4. Re-embeds and updates the embedding in-place
/// Returns the number of chunks re-embedded.
pub(crate) fn enrichment_pass(
    store: &Store,
    embedder: &Embedder,
    model_config: &cqs::embedder::ModelConfig,
    quiet: bool,
) -> Result<usize> {
    let _span = tracing::info_span!("enrichment_pass").entered();

    // Step 1: Count chunks for IDF computation
    let stats = store.stats().context("Failed to get index stats")?;
    let total_chunks = stats.total_chunks as f32;
    if total_chunks < 1.0 {
        return Ok(0);
    }

    // Step 2: Build callee caller-count map for IDF-style filtering.
    // A callee called by >=10% of unique callers is a utility — suppress it.
    let callee_freq = store
        .callee_caller_counts()
        .context("Failed to compute callee frequencies")?;
    let callee_doc_freq: HashMap<String, f32> = callee_freq
        .into_iter()
        .map(|(name, count)| (name, count as f32 / total_chunks))
        .collect();

    // Step 3: Iterate chunks in pages, collect those needing enrichment
    let mut enriched_count = 0usize;
    let mut cursor = 0i64;
    const ENRICHMENT_PAGE_SIZE: usize = 500;

    // Track name frequency — ambiguous names (appearing in multiple files)
    // are skipped to avoid merging callers from different functions. (RB-B1)
    let identities = store
        .all_chunk_identities()
        .context("Failed to load chunk identities")?;
    let mut name_file_count: HashMap<&str, usize> = HashMap::new();
    for ci in &identities {
        *name_file_count.entry(ci.name.as_str()).or_insert(0) += 1;
    }

    let progress = if quiet {
        ProgressBar::hidden()
    } else {
        let pb = ProgressBar::new(stats.total_chunks);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{bar:40}] {pos}/{len} enriching ({eta})")
                .expect("valid progress template")
                .progress_chars("=>-"),
        );
        pb
    };

    // (chunk_id, enriched_nl, enrichment_hash)
    let mut embed_batch: Vec<(String, String, String)> = Vec::new();
    // SHL-V1.30-1: model-aware batch size so nomic-coderank (768 dim,
    // 2048 seq) doesn't OOM at batch=64 on an 8 GB GPU. CQS_EMBED_BATCH_SIZE
    // override is still honoured inside `embed_batch_size_for`.
    let enrich_embed_batch: usize = super::pipeline::embed_batch_size_for(model_config);
    let mut skipped_count = 0usize;

    // Pre-fetch all LLM summaries once before the page loop (PERF-18).
    // Single query instead of per-page batched fetches.
    // RM-25: Intentional full pre-load — summaries and HyDE predictions are ~100 bytes each,
    // so even 100k chunks uses ~20MB. The alternative (paged lookups) would require N SQLite
    // round trips during the enrichment loop. This is the right trade-off for batch processing.
    let all_summaries = match store.get_all_summaries("summary") {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to pre-fetch LLM summaries for enrichment");
            HashMap::new()
        }
    };

    let all_hyde = match store.get_all_summaries("hyde") {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to pre-fetch hyde predictions for enrichment");
            HashMap::new()
        }
    };

    // #966: normalize whitespace once per content_hash. Summaries/hyde are keyed
    // by content_hash so the same physical string was being re-normalized by every
    // chunk sharing that content. On a 100k-chunk reindex the per-chunk
    // split_whitespace().collect::<Vec<_>>().join(" ") produced ~1KB of churn each
    // (~100MB allocator pressure). Pre-computing in two HashMaps (~20MB) eliminates
    // that churn and the per-chunk hash function now only borrows &str.
    let all_summaries_norm: HashMap<String, String> = all_summaries
        .iter()
        .map(|(k, v)| (k.clone(), normalize_ws(v)))
        .collect();
    let all_hyde_norm: HashMap<String, String> = all_hyde
        .iter()
        .map(|(k, v)| (k.clone(), normalize_ws(v)))
        .collect();

    // PERF-29: Pre-fetch all enrichment hashes once instead of per-page queries.
    // Same trade-off as summaries above: ~32 bytes per hash × N chunks is small.
    let all_enrichment_hashes = match store.get_all_enrichment_hashes() {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to pre-fetch enrichment hashes");
            HashMap::new()
        }
    };

    // Wrap loop in closure so progress bar is always cleaned up on error
    let result: Result<usize> = (|| {
        loop {
            let (chunks, next_cursor) = store
                .chunks_paged(cursor, ENRICHMENT_PAGE_SIZE)
                .context("Failed to page chunks")?;
            if chunks.is_empty() {
                break;
            }
            cursor = next_cursor;

            // Batch-fetch callers/callees for this page only (~500 names).
            // Avoids pre-loading the full call graph (~105MB at 50k chunks).
            let page_names: Vec<&str> = chunks.iter().map(|cs| cs.name.as_str()).collect();
            tracing::debug!(
                page = cursor,
                names = page_names.len(),
                "Loading callers/callees for enrichment page"
            );
            let callers_map = store
                .get_callers_full_batch(&page_names)
                .context("Failed to batch-fetch callers for page")?;
            let callees_map = store
                .get_callees_full_batch(&page_names)
                .context("Failed to batch-fetch callees for page")?;

            for cs in &chunks {
                progress.inc(1);

                let callers = callers_map.get(&cs.name);
                let callees = callees_map.get(&cs.name);

                let has_callers = callers.is_some_and(|v| !v.is_empty());
                let has_callees = callees.is_some_and(|v| !v.is_empty());
                let summary = all_summaries.get(&cs.content_hash).map(|s| s.as_str());
                let hyde = all_hyde.get(&cs.content_hash).map(|s| s.as_str());
                // #966: pre-normalized variants used for hashing only.
                // generate_nl_with_call_context_and_summary() still sees the raw
                // summary/hyde so NL output is unchanged.
                let summary_norm = all_summaries_norm.get(&cs.content_hash).map(|s| s.as_str());
                let hyde_norm = all_hyde_norm.get(&cs.content_hash).map(|s| s.as_str());

                // Skip chunks with nothing to add: no call context, no summary, no hyde
                if !has_callers && !has_callees && summary.is_none() && hyde.is_none() {
                    continue;
                }

                // Skip ambiguous names — functions like `new`, `parse`, `build`
                // appear in multiple chunks and would get merged callers from
                // unrelated functions. (RB-B1)
                // But still process if they have a summary or hyde (neither depends on call graph)
                if name_file_count.get(cs.name.as_str()).copied().unwrap_or(0) > 1
                    && summary.is_none()
                    && hyde.is_none()
                {
                    continue;
                }

                // PERF-20/21: These clone caller/callee names into CallContext.
                // Borrowing would require lifetime parameters through CallContext → generate_nl,
                // cascading across 5+ modules. At ~5 callers + ~5 callees per chunk, these
                // clones are negligible (~500 bytes) compared to the embedding cost (~3ms each).
                let ctx = cqs::CallContext {
                    callers: callers
                        .map(|v| v.iter().map(|c| c.name.clone()).collect())
                        .unwrap_or_default(),
                    callees: callees
                        .map(|v| v.iter().map(|(name, _)| name.clone()).collect())
                        .unwrap_or_default(),
                };

                // Compute enrichment hash from post-filtered call context + summary (RT-DATA-2, SQ-6).
                // #966: pass pre-normalized summary/hyde so the hash function can
                // just stream bytes via blake3::Hasher instead of re-normalizing
                // and accumulating a String per chunk.
                let enrichment_hash = compute_enrichment_hash_with_summary(
                    &ctx,
                    &callee_doc_freq,
                    summary_norm,
                    hyde_norm,
                );

                // Skip if already enriched with the same call context + summary
                if let Some(stored) = all_enrichment_hashes.get(&cs.id) {
                    if *stored == enrichment_hash {
                        skipped_count += 1;
                        continue;
                    }
                }

                let chunk: cqs::parser::Chunk = cs.into();
                let enriched_nl = cqs::generate_nl_with_call_context_and_summary(
                    &chunk,
                    &ctx,
                    &callee_doc_freq,
                    5, // max callers
                    5, // max callees
                    summary,
                    hyde,
                );

                embed_batch.push((cs.id.clone(), enriched_nl, enrichment_hash));

                // Flush batch when full
                if embed_batch.len() >= enrich_embed_batch {
                    enriched_count += flush_enrichment_batch(store, embedder, &mut embed_batch)?;
                }
            }
        }

        // Flush remaining
        if !embed_batch.is_empty() {
            enriched_count += flush_enrichment_batch(store, embedder, &mut embed_batch)?;
        }

        Ok(enriched_count)
    })();

    progress.finish_and_clear();

    let enriched_count = result?;

    tracing::info!(enriched_count, skipped_count, "Enrichment pass complete");
    if !quiet {
        if skipped_count > 0 {
            eprintln!(
                "Enriched {} chunks with call graph context ({} already up-to-date)",
                enriched_count, skipped_count
            );
        } else {
            eprintln!("Enriched {} chunks with call graph context", enriched_count);
        }
    }

    Ok(enriched_count)
}

/// RB-6 / #966: normalize whitespace so LLM-generated strings with
/// non-deterministic leading/trailing/internal whitespace collapse to the
/// same canonical form before hashing.
///
/// Factored out of `compute_enrichment_hash_with_summary` so the normalized
/// form can be cached once per `content_hash` instead of recomputed per
/// chunk in the reindex hot path.
fn normalize_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Compute enrichment hash including optional LLM summary (SQ-6).
/// Extends `compute_enrichment_hash` to also include the summary text.
/// If the summary changes, the hash changes, triggering re-embedding.
///
/// #966: `summary` and `hyde` MUST ALREADY be whitespace-normalized by the
/// caller (see `normalize_ws`). In production the caller normalizes once
/// per unique `content_hash` and reuses the result across every chunk that
/// shares content, avoiding ~100MB of per-chunk String churn on 100k-chunk
/// reindexes. Tests call `normalize_ws` inline.
///
/// Uses `blake3::Hasher` streaming — no intermediate `String` accumulator.
/// The byte layout must stay identical to the pre-#966 `write!` version so
/// existing enrichment_hash values in the store remain valid (otherwise
/// every reindex would invalidate every cache entry). Layout is:
///     "c:{caller}|" per sorted caller
///     "e:{callee}|" per sorted filtered callee
///     "s:{normalized_summary}"   (no trailing separator, only if present)
///     "h:{normalized_hyde}"      (no trailing separator, only if present)
/// Hash is truncated to the first 32 hex chars to match pre-#966 output.
fn compute_enrichment_hash_with_summary(
    ctx: &cqs::CallContext,
    callee_doc_freq: &HashMap<String, f32>,
    summary: Option<&str>,
    hyde: Option<&str>,
) -> String {
    let mut hasher = blake3::Hasher::new();

    let mut callers: Vec<&str> = ctx.callers.iter().map(|s| s.as_str()).collect();
    callers.sort_unstable();
    for c in &callers {
        hasher.update(b"c:");
        hasher.update(c.as_bytes());
        hasher.update(b"|");
    }

    let mut callees: Vec<&str> = ctx
        .callees
        .iter()
        // DS-22: Cast to f64 for boundary comparison to avoid f32 non-determinism.
        .filter(|name| {
            (callee_doc_freq.get(name.as_str()).copied().unwrap_or(0.0) as f64) < 0.1_f64
        })
        .map(|s| s.as_str())
        .collect();
    callees.sort_unstable();
    for c in &callees {
        hasher.update(b"e:");
        hasher.update(c.as_bytes());
        hasher.update(b"|");
    }

    if let Some(s) = summary {
        hasher.update(b"s:");
        hasher.update(s.as_bytes());
    }

    if let Some(h) = hyde {
        hasher.update(b"h:");
        hasher.update(h.as_bytes());
    }

    let hash = hasher.finalize();
    hash.to_hex()[..32].to_string()
}

/// Embed a batch of enriched NL descriptions and update their embeddings in the store.
fn flush_enrichment_batch(
    store: &Store,
    embedder: &Embedder,
    batch: &mut Vec<(String, String, String)>,
) -> Result<usize> {
    let _span = tracing::info_span!("flush_enrichment_batch", count = batch.len()).entered();
    let texts: Vec<&str> = batch.iter().map(|(_, nl, _)| nl.as_str()).collect();
    let expected = texts.len();
    let embeddings = embedder
        .embed_documents(&texts)
        .context("Failed to embed enriched NL batch")?;

    anyhow::ensure!(
        embeddings.len() == expected,
        "Embedding count mismatch: expected {}, got {}",
        expected,
        embeddings.len()
    );

    // Build updates from batch without draining — only clear after successful write
    let updates: Vec<(String, Embedding, Option<String>)> = batch
        .iter()
        .zip(embeddings)
        .map(|((id, _, hash), emb)| (id.clone(), emb, Some(hash.clone())))
        .collect();

    store
        .update_embeddings_with_hashes_batch(&updates)
        .context("Failed to update enriched embeddings")?;

    let count = updates.len();
    batch.clear(); // clear only after successful write
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to build a CallContext with given callers and callees.
    fn make_ctx(callers: &[&str], callees: &[&str]) -> cqs::CallContext {
        cqs::CallContext {
            callers: callers.iter().map(|s| s.to_string()).collect(),
            callees: callees.iter().map(|s| s.to_string()).collect(),
        }
    }

    // ---------- enrichment hash determinism (#665) ----------

    #[test]
    fn enrichment_hash_deterministic_same_inputs() {
        let ctx = make_ctx(&["caller_a", "caller_b"], &["callee_x", "callee_y"]);
        let freq: HashMap<String, f32> = HashMap::new();
        let summary = Some("Processes raw data");

        let h1 = compute_enrichment_hash_with_summary(&ctx, &freq, summary, None);
        let h2 = compute_enrichment_hash_with_summary(&ctx, &freq, summary, None);
        assert_eq!(h1, h2, "Same inputs must produce identical hashes");
    }

    #[test]
    fn enrichment_hash_deterministic_regardless_of_caller_order() {
        // Callers are sorted internally, so insertion order shouldn't matter.
        let ctx_ab = make_ctx(&["caller_a", "caller_b"], &["callee_x"]);
        let ctx_ba = make_ctx(&["caller_b", "caller_a"], &["callee_x"]);
        let freq: HashMap<String, f32> = HashMap::new();

        let h1 = compute_enrichment_hash_with_summary(&ctx_ab, &freq, None, None);
        let h2 = compute_enrichment_hash_with_summary(&ctx_ba, &freq, None, None);
        assert_eq!(h1, h2, "Caller order must not affect hash");
    }

    #[test]
    fn enrichment_hash_deterministic_regardless_of_callee_order() {
        let ctx_xy = make_ctx(&[], &["callee_x", "callee_y"]);
        let ctx_yx = make_ctx(&[], &["callee_y", "callee_x"]);
        let freq: HashMap<String, f32> = HashMap::new();

        let h1 = compute_enrichment_hash_with_summary(&ctx_xy, &freq, None, None);
        let h2 = compute_enrichment_hash_with_summary(&ctx_yx, &freq, None, None);
        assert_eq!(h1, h2, "Callee order must not affect hash");
    }

    #[test]
    fn enrichment_hash_changes_with_different_callers() {
        let ctx1 = make_ctx(&["caller_a"], &["callee_x"]);
        let ctx2 = make_ctx(&["caller_b"], &["callee_x"]);
        let freq: HashMap<String, f32> = HashMap::new();

        let h1 = compute_enrichment_hash_with_summary(&ctx1, &freq, None, None);
        let h2 = compute_enrichment_hash_with_summary(&ctx2, &freq, None, None);
        assert_ne!(h1, h2, "Different callers must produce different hashes");
    }

    #[test]
    fn enrichment_hash_changes_with_summary() {
        let ctx = make_ctx(&["caller_a"], &["callee_x"]);
        let freq: HashMap<String, f32> = HashMap::new();

        let h_none = compute_enrichment_hash_with_summary(&ctx, &freq, None, None);
        let h_some = compute_enrichment_hash_with_summary(&ctx, &freq, Some("a summary"), None);
        assert_ne!(h_none, h_some, "Adding a summary must change the hash");
    }

    #[test]
    fn enrichment_hash_filters_high_freq_callees() {
        // "log" is at 15% frequency (above 10% threshold), should be excluded from hash.
        let ctx = make_ctx(&[], &["log", "rare_fn"]);
        let mut freq: HashMap<String, f32> = HashMap::new();
        freq.insert("log".to_string(), 0.15);
        freq.insert("rare_fn".to_string(), 0.02);

        // Compare against a context that only has "rare_fn" — should match
        // because "log" is filtered out of the hash.
        let ctx_without_log = make_ctx(&[], &["rare_fn"]);
        let empty_freq: HashMap<String, f32> = HashMap::new();

        let h_with = compute_enrichment_hash_with_summary(&ctx, &freq, None, None);
        let h_without =
            compute_enrichment_hash_with_summary(&ctx_without_log, &empty_freq, None, None);
        assert_eq!(
            h_with, h_without,
            "High-frequency callees (>=10% IDF) must be excluded from hash"
        );
    }

    #[test]
    fn enrichment_hash_changes_with_hyde() {
        let ctx = make_ctx(&["caller_a"], &[]);
        let freq: HashMap<String, f32> = HashMap::new();

        let h_none = compute_enrichment_hash_with_summary(&ctx, &freq, None, None);
        let h_hyde = compute_enrichment_hash_with_summary(&ctx, &freq, None, Some("how to search"));
        assert_ne!(h_none, h_hyde, "Adding hyde must change the hash");
    }

    // ---------- #966: byte-identical snapshot test ----------

    /// Reference implementation matching the PRE-#966 `write!`-into-String
    /// accumulator. Kept only in tests so we can prove the streaming
    /// `blake3::Hasher` version produces the exact same bytes for the same
    /// inputs. If this ever diverges from `compute_enrichment_hash_with_summary`,
    /// every cached `enrichment_hash` in production stores is invalidated
    /// and the next reindex re-embeds every chunk. Don't change this.
    fn reference_hash_pre_966(
        ctx: &cqs::CallContext,
        callee_doc_freq: &HashMap<String, f32>,
        summary: Option<&str>,
        hyde: Option<&str>,
    ) -> String {
        use std::fmt::Write;
        let mut input = String::new();

        let mut callers: Vec<&str> = ctx.callers.iter().map(|s| s.as_str()).collect();
        callers.sort_unstable();
        for c in &callers {
            let _ = write!(input, "c:{c}|");
        }

        let mut callees: Vec<&str> = ctx
            .callees
            .iter()
            .filter(|name| {
                (callee_doc_freq.get(name.as_str()).copied().unwrap_or(0.0) as f64) < 0.1_f64
            })
            .map(|s| s.as_str())
            .collect();
        callees.sort_unstable();
        for c in &callees {
            let _ = write!(input, "e:{c}|");
        }

        if let Some(s) = summary {
            let norm: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
            let _ = write!(input, "s:{norm}");
        }

        if let Some(h) = hyde {
            let norm: String = h.split_whitespace().collect::<Vec<_>>().join(" ");
            let _ = write!(input, "h:{norm}");
        }

        let hash = blake3::hash(input.as_bytes());
        hash.to_hex()[..32].to_string()
    }

    #[test]
    fn enrichment_hash_byte_identical_streaming() {
        // Fixture covers all four input categories: sorted callers,
        // filtered + sorted callees (one suppressed by IDF), and both
        // summary + hyde with non-trivial whitespace that exercises
        // the normalization path.
        let ctx = make_ctx(&["beta", "alpha"], &["rare_fn", "log"]);
        let mut freq: HashMap<String, f32> = HashMap::new();
        freq.insert("log".to_string(), 0.15); // filtered (>=10%)
        freq.insert("rare_fn".to_string(), 0.02); // kept
        let summary_raw = "  hello\tworld ";
        let hyde_raw = " how\n to search ";

        // Production pre-normalizes before calling; mirror that here.
        let summary_norm = normalize_ws(summary_raw);
        let hyde_norm = normalize_ws(hyde_raw);

        let streaming = compute_enrichment_hash_with_summary(
            &ctx,
            &freq,
            Some(&summary_norm),
            Some(&hyde_norm),
        );

        // The reference function re-normalizes internally (matching the
        // pre-#966 behavior), so it takes the raw strings. Feeding it
        // the already-normalized strings would be equivalent because
        // normalize_ws is idempotent on its own output.
        let reference = reference_hash_pre_966(&ctx, &freq, Some(summary_raw), Some(hyde_raw));

        assert_eq!(
            streaming, reference,
            "streaming blake3::Hasher must produce byte-identical output to the pre-#966 accumulator"
        );

        // Hardcoded snapshot so a future refactor that silently drifts
        // the byte layout (e.g. drops a `|` separator) fails loudly
        // even if someone also breaks the reference function.
        const EXPECTED: &str = "7d6ef6b83f8700e398ac9c3823e61dfd";
        assert_eq!(
            streaming, EXPECTED,
            "enrichment hash byte layout drifted — cached hashes in production stores are now invalid"
        );
    }

    #[test]
    fn enrichment_hash_normalize_ws_idempotent() {
        // normalize_ws must be idempotent so pre-normalizing at the call
        // site produces the same hash as normalizing per-chunk.
        let raw = "  foo\tbar\n\n baz  ";
        let once = normalize_ws(raw);
        let twice = normalize_ws(&once);
        assert_eq!(once, twice, "normalize_ws must be idempotent");
        assert_eq!(once, "foo bar baz");
    }
}
