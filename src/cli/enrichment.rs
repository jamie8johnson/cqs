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
///
/// After the main pipeline populates the `function_calls` table, this pass:
/// 1. Computes callee document frequency (IDF) for stopword filtering
/// 2. Iterates all chunks in pages
/// 3. For each chunk with callers or callees, regenerates NL with call context
/// 4. Re-embeds and updates the embedding in-place
///
/// Returns the number of chunks re-embedded.
pub(crate) fn enrichment_pass(store: &Store, embedder: &Embedder, quiet: bool) -> Result<usize> {
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

    // Collect all chunk names for batch caller/callee lookup.
    // Track name frequency — ambiguous names (appearing in multiple files)
    // are skipped to avoid merging callers from different functions. (RB-B1)
    let identities = store
        .all_chunk_identities()
        .context("Failed to load chunk identities")?;
    let mut name_file_count: HashMap<String, usize> = HashMap::new();
    for ci in &identities {
        *name_file_count.entry(ci.name.clone()).or_insert(0) += 1;
    }
    let all_names: Vec<&str> = identities.iter().map(|ci| ci.name.as_str()).collect();

    // Batch-fetch all callers and callees
    let callers_map = store
        .get_callers_full_batch(&all_names)
        .context("Failed to batch-fetch callers")?;
    let callees_map = store
        .get_callees_full_batch(&all_names)
        .context("Failed to batch-fetch callees")?;

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
    const ENRICH_EMBED_BATCH: usize = 64;
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

            for cs in &chunks {
                progress.inc(1);

                let callers = callers_map.get(&cs.name);
                let callees = callees_map.get(&cs.name);

                let has_callers = callers.is_some_and(|v| !v.is_empty());
                let has_callees = callees.is_some_and(|v| !v.is_empty());
                let summary = all_summaries.get(&cs.content_hash).map(|s| s.as_str());
                let hyde = all_hyde.get(&cs.content_hash).map(|s| s.as_str());

                // Skip chunks with nothing to add: no call context, no summary, no hyde
                if !has_callers && !has_callees && summary.is_none() && hyde.is_none() {
                    continue;
                }

                // Skip ambiguous names — functions like `new`, `parse`, `build`
                // appear in multiple chunks and would get merged callers from
                // unrelated functions. (RB-B1)
                // But still process if they have a summary or hyde (neither depends on call graph)
                if name_file_count.get(&cs.name).copied().unwrap_or(0) > 1
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
                let enrichment_hash =
                    compute_enrichment_hash_with_summary(&ctx, &callee_doc_freq, summary, hyde);

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
                if embed_batch.len() >= ENRICH_EMBED_BATCH {
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

/// Compute enrichment hash including optional LLM summary (SQ-6).
///
/// Extends `compute_enrichment_hash` to also include the summary text.
/// If the summary changes, the hash changes, triggering re-embedding.
fn compute_enrichment_hash_with_summary(
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
        // DS-22: Cast to f64 for boundary comparison to avoid f32 non-determinism.
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
        let _ = write!(input, "s:{s}");
    }

    if let Some(h) = hyde {
        let _ = write!(input, "h:{h}");
    }

    let hash = blake3::hash(input.as_bytes());
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
