//! Shared embedding-reuse resolution for the indexing side.
//!
//! Both the bulk pipeline (`embedding::prepare_for_embedding`) and the
//! watch/daemon incremental path (`watch::reindex::reindex_files`) need the
//! same three-step reuse decision: read the project-scoped global embedding
//! cache, fall back to the per-slot store cache, then split chunks into
//! "reuse a cached embedding" vs "embed fresh". Keeping the decision in one
//! place means a change to reuse semantics (keys, purposes, precedence) has
//! exactly one edit site instead of a per-caller copy that can drift.
//!
//! This module owns the reuse DECISION only. Each caller keeps its own
//! batching/threading/windowing and maps the returned index split into its own
//! output shape (the bulk pipeline wants owned `(Chunk, Embedding)`; the watch
//! path wants `(usize, &Chunk)` to rebuild original order). The function takes
//! chunks by slice and returns indices into that slice so neither ownership
//! model is forced on the other.

use std::collections::HashMap;

use cqs::{Chunk, Embedding, Store};

/// The reuse-resolution key for a chunk.
///
/// Cache reuse is keyed by `canonical_hash` (comment-/whitespace-normalized
/// content, schema v28) — NOT `content_hash`. A comment-only or formatting-only
/// edit changes `content_hash` (store identity) but leaves `canonical_hash`
/// stable, so the embedding is reused instead of re-embedding the whole corpus.
/// `content_hash` is still what's persisted as the row's identity; only the
/// lookup key changes here.
///
/// A chunk with an empty `canonical_hash` (a hydrated round-trip Chunk —
/// shouldn't occur on the index path, but guard anyway) falls back to its
/// `content_hash` so it still gets a usable, content-exact key — the
/// NULL/empty-canonical fallback every reuse site must share; having it in
/// one place is the point of this module.
///
/// Borrows from the chunk so both the read path ([`resolve_reuse`]) and the
/// cache write-back sites (bulk GPU/CPU stages, watch reindex) share the SAME
/// key function without per-chunk `String` allocation on hot paths.
pub(crate) fn canon_key_ref(c: &Chunk) -> &str {
    if c.canonical_hash.is_empty() {
        &c.content_hash
    } else {
        &c.canonical_hash
    }
}

/// The result of resolving reuse for a batch of chunks.
///
/// `cached` and `to_embed` hold indices into the original chunk slice passed to
/// [`resolve_reuse`]. The caller maps these back to chunks (owned or borrowed)
/// in whatever shape its downstream stage needs. The split is exhaustive and
/// disjoint: every input index appears in exactly one of the two vectors.
pub(crate) struct ReuseSplit {
    /// `(chunk_index, reused_embedding)` for chunks satisfied by the global or
    /// store cache. Order follows the input chunk order.
    pub cached: Vec<(usize, Embedding)>,
    /// Indices of chunks that need a fresh embedding (cache miss). Order follows
    /// the input chunk order.
    pub to_embed: Vec<usize>,
    /// Number of hits served by the global (cross-slot) cache, for the
    /// `global_hits` / `store_hits` split in the caller's tracing line.
    pub global_hits: usize,
}

/// Resolve embedding reuse for a batch of chunks.
///
/// Runs the shared three-step chain:
/// 1. Read the project-scoped global cache (`global_cache`) by canonical key.
/// 2. For the misses, read the per-slot store cache — but only when the
///    embedder dim matches the store dim (a model swap mid-index leaves stale
///    vectors), and only for canonical hashes the global cache didn't satisfy.
/// 3. Walk the chunks in order and take ownership of each reused embedding via
///    `.remove()`, falling through to `to_embed` on a miss.
///
/// `dim` is the embedder's embedding dimension and `model_fingerprint` its
/// model fingerprint (computed by each caller, and ONLY when a global cache is
/// present — the fingerprint's first computation streams blake3 over the full
/// ONNX model file, so callers gate it on `global_cache.is_some()`). Both are
/// passed in rather than re-derived so this stays decoupled from the
/// `Embedder` type. The fingerprint is only consumed by the global-cache
/// branch; `None` with `global_cache: Some(..)` skips that branch.
///
/// # Errors
///
/// A per-slot STORE-cache read failure returns `Err` — the watch path
/// propagates it (aborting the cycle so the daemon retries next tick with the
/// error visible) while the bulk pipeline catches it, warns, and degrades to
/// re-embedding the batch. The GLOBAL-cache read stays best-effort internally:
/// a read error there logs and degrades to the store cache, never blocks.
///
/// **Duplicate-key fallthrough contract (load-bearing, tested):** two chunks
/// that share a canonical key within one batch (identical after normalization —
/// rare, implies duplicate content across files) are NOT both served from one
/// cached vector. The first `.remove()` consumes the slot; the second falls
/// through to `to_embed`. One cached embedding satisfies exactly one chunk.
pub(crate) fn resolve_reuse(
    chunks: &[Chunk],
    store: &Store,
    global_cache: Option<&cqs::cache::EmbeddingCache>,
    dim: usize,
    model_fingerprint: Option<&str>,
) -> anyhow::Result<ReuseSplit> {
    let _span = tracing::debug_span!("resolve_reuse", chunks = chunks.len()).entered();

    let hashes: Vec<&str> = chunks.iter().map(canon_key_ref).collect();

    // Step 1: global (project-scoped, cross-slot) cache. Best-effort — a read
    // error logs and degrades to the store cache, never blocks indexing.
    //
    // Pre-enrichment helper writes/reads the post-enrichment `Embedding`
    // purpose. EmbeddingBase has no producer here yet — when enrichment caching
    // lands it will own its own purpose.
    let mut global_hits: HashMap<String, Embedding> = HashMap::new();
    if let (Some(cache), Some(fingerprint)) = (global_cache, model_fingerprint) {
        match cache.read_batch(
            &hashes,
            fingerprint,
            cqs::cache::CachePurpose::Embedding,
            dim,
        ) {
            Ok(hits) => {
                let total = hits.len();
                let mut dropped = 0usize;
                for (hash, emb_vec) in hits {
                    // Same finiteness guard as the store-cache path
                    // (`get_embeddings_by_canonical_hashes`): reject NaN/Inf
                    // before they can poison HNSW build or query paths. A
                    // dropped hit falls through to re-embedding, so warn —
                    // a silent drop hides cache corruption indefinitely.
                    match Embedding::try_new(emb_vec) {
                        Ok(emb) => {
                            global_hits.insert(hash, emb);
                        }
                        Err(e) => {
                            dropped += 1;
                            tracing::warn!(
                                hash = %hash,
                                error = %e,
                                "Non-finite embedding values (NaN/Inf) in global cache, \
                                 re-embedding — run 'cqs cache clear' to purge corrupt entries"
                            );
                        }
                    }
                }
                if total > 0 {
                    tracing::debug!(hits = total, dropped, "Global cache hits");
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "Global cache read failed (best-effort)");
            }
        }
    }

    // Step 2: per-slot store cache. Only query for the hashes the global cache
    // didn't satisfy (a warm rebuild that hits global for everything skips the
    // bind-heavy SELECT), and only when the embedder dim matches store dim — a
    // model swap means the stored vectors belong to a different model.
    //
    // Store-side reuse is also keyed by canonical_hash (v28). Rows whose
    // canonical_hash IS NULL (pre-v28) are skipped by the helper, so they
    // re-embed on first touch — a clean cache miss, never a wrong hit.
    //
    // A read failure here propagates as `Err` — see the function doc. The
    // watch path needs the failure visible (a persistent SQLite error must not
    // silently become a total cache miss that re-embeds the corpus on GPU each
    // cycle); the bulk pipeline catches it and degrades.
    let mut store_hits: HashMap<String, Embedding> = if dim == store.dim() {
        let missed: Vec<&str> = hashes
            .iter()
            .copied()
            .filter(|h| !global_hits.contains_key(*h))
            .collect();
        if missed.is_empty() {
            HashMap::new()
        } else {
            store
                .get_embeddings_by_canonical_hashes(&missed)
                .map_err(|e| {
                    anyhow::anyhow!(e)
                        .context("Failed to fetch cached embeddings by canonical hash")
                })?
        }
    } else {
        tracing::info!(
            store_dim = store.dim(),
            embedder_dim = dim,
            "Skipping store embedding cache (dimension mismatch — model switch)"
        );
        HashMap::new()
    };

    // Step 3: split. Take ownership of each reused embedding via `.remove()`
    // (global cache first, then store cache) so cached vectors aren't cloned.
    // A duplicate canonical key within one batch falls through to `to_embed` on
    // the second hit because the first `.remove()` consumed the slot — one
    // cached embedding satisfies exactly one chunk, preserving the duplicate-
    // fallthrough contract under the canonical key.
    let global_hits_total = global_hits.len();
    let mut cached: Vec<(usize, Embedding)> = Vec::new();
    let mut to_embed: Vec<usize> = Vec::new();
    for (i, key) in hashes.iter().enumerate() {
        if let Some(emb) = global_hits.remove(*key) {
            cached.push((i, emb));
        } else if let Some(emb) = store_hits.remove(*key) {
            cached.push((i, emb));
        } else {
            to_embed.push(i);
        }
    }

    Ok(ReuseSplit {
        cached,
        to_embed,
        global_hits: global_hits_total,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cqs::language::{ChunkType, Language};
    use cqs::store::ModelInfo;
    use std::path::PathBuf;

    fn chunk_with(id: &str, content: &str, canonical: &str) -> Chunk {
        Chunk {
            id: id.to_string(),
            file: PathBuf::from("test.rs"),
            language: Language::Rust,
            chunk_type: ChunkType::Function,
            name: id.to_string(),
            signature: String::new(),
            content: content.to_string(),
            doc: None,
            line_start: 1,
            line_end: 10,
            content_hash: blake3::hash(content.as_bytes()).to_hex().to_string(),
            canonical_hash: canonical.to_string(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        }
    }

    fn open_store(dim: usize) -> (tempfile::TempDir, Store) {
        let tmp = tempfile::TempDir::new().unwrap();
        let store_path = tmp.path().join(cqs::INDEX_DB_FILENAME);
        let mut store = Store::open(&store_path).unwrap();
        store.init(&ModelInfo::new("test/m", dim)).unwrap();
        // `Store::open` probes the default dim (768); `init`'s ModelInfo records
        // the model row but doesn't reset the in-memory `dim` field, which gates
        // `upsert_chunks_batch` and `get_embeddings_by_canonical_hashes`. Pin it
        // to the test dim so the seeded embeddings/lookups all agree.
        store.set_dim(dim);
        (tmp, store)
    }

    /// NULL/empty-canonical fallback: a chunk with no canonical_hash keys on its
    /// content_hash, so a store row written under that content_hash is reused.
    #[test]
    fn null_canonical_falls_back_to_content_hash() {
        let dim = 8;
        let (_tmp, store) = open_store(dim);

        // Seed a chunk whose canonical_hash IS empty. The store upsert persists
        // canonical_hash; with empty canonical the row's canonical equals
        // content_hash via the same fallback, so `get_embeddings_by_canonical_hashes`
        // can find it under the content_hash key.
        let mut seeded = chunk_with("seed", "fn seeded() {}", "");
        // The store binds an empty canonical_hash as NULL (async_helpers.rs),
        // so the store-side canonical lookup can never hit for such chunks; this
        // test pins the READ-side fallback: canon_key_ref uses content_hash when
        // canonical_hash is empty, letting the global cache still serve.
        seeded.canonical_hash = seeded.content_hash.clone();
        let mut v = vec![0.0_f32; dim];
        v[0] = 1.0;
        store
            .upsert_chunks_batch(&[(seeded.clone(), Embedding::new(v))], Some(0))
            .unwrap();

        // Lookup chunk: same content, empty canonical → canon_key_ref == content_hash.
        let lookup = chunk_with("lookup", "fn seeded() {}", "");
        assert!(lookup.canonical_hash.is_empty());
        assert_eq!(canon_key_ref(&lookup), lookup.content_hash);

        let split = resolve_reuse(&[lookup], &store, None, dim, None).unwrap();
        assert_eq!(
            split.cached.len(),
            1,
            "empty-canonical chunk reused via content_hash fallback"
        );
        assert!(split.to_embed.is_empty());
        assert_eq!(split.cached[0].0, 0);
    }

    /// Duplicate-fallthrough contract: two chunks sharing a canonical key get one
    /// cached slot; the second falls through to to_embed.
    #[test]
    fn duplicate_canonical_key_falls_through_to_to_embed() {
        let dim = 8;
        let (_tmp, store) = open_store(dim);

        // One stored embedding under canonical key "dup".
        let mut seeded = chunk_with("seed", "fn a() {}", "dup");
        seeded.canonical_hash = "dup".to_string();
        let mut v = vec![0.0_f32; dim];
        v[1] = 1.0;
        store
            .upsert_chunks_batch(&[(seeded.clone(), Embedding::new(v))], Some(0))
            .unwrap();

        // Two lookup chunks that both key on "dup".
        let a = chunk_with("a", "fn a() {}", "dup");
        let b = chunk_with("b", "fn b_but_same_canon() {}", "dup");
        assert_eq!(canon_key_ref(&a), "dup");
        assert_eq!(canon_key_ref(&b), "dup");

        let split = resolve_reuse(&[a, b], &store, None, dim, None).unwrap();
        assert_eq!(
            split.cached.len(),
            1,
            "one cached embedding satisfies exactly one slot"
        );
        assert_eq!(
            split.to_embed.len(),
            1,
            "duplicate-canon second chunk falls through to to_embed"
        );
        // First chunk (index 0) wins the slot; second (index 1) re-embeds.
        assert_eq!(split.cached[0].0, 0);
        assert_eq!(split.to_embed[0], 1);
    }

    /// Fresh batch (no cache, no store rows): everything routes to to_embed.
    #[test]
    fn fresh_batch_all_to_embed() {
        let dim = 8;
        let (_tmp, store) = open_store(dim);
        let chunks = vec![
            chunk_with("c1", "fn one() {}", "k1"),
            chunk_with("c2", "fn two() {}", "k2"),
        ];
        let split = resolve_reuse(&chunks, &store, None, dim, None).unwrap();
        assert!(split.cached.is_empty());
        assert_eq!(split.to_embed, vec![0, 1]);
        assert_eq!(split.global_hits, 0);
    }

    /// A corrupt (non-finite) global-cache vector must be dropped (with a
    /// warn) and the chunk routed to `to_embed` — never served as a hit, and
    /// never counted in `global_hits`.
    #[test]
    fn non_finite_global_cache_entry_falls_through_to_embed() {
        let dim = 8;
        let (_tmp, store) = open_store(dim);

        let cache_dir = tempfile::TempDir::new().unwrap();
        let cache_path = cache_dir.path().join("embeddings_cache.db");
        let cache = cqs::cache::EmbeddingCache::open(&cache_path).unwrap();
        let fp = "test-fingerprint";

        let chunk = chunk_with("a", "fn a() {}", "k1");
        let key = canon_key_ref(&chunk).to_string();

        // `write_batch` rejects non-finite vectors at write time, so seed a
        // valid row first, then corrupt the stored blob through a direct
        // connection — modeling on-disk corruption or a cache written before
        // the write-side finiteness guard existed.
        let written = cache
            .write_batch_owned(
                &[(key.clone(), vec![0.5_f32; dim])],
                fp,
                cqs::cache::CachePurpose::Embedding,
                dim,
            )
            .unwrap();
        assert_eq!(written, 1);

        let mut nan_blob = Vec::with_capacity(dim * 4);
        for _ in 0..dim {
            nan_blob.extend_from_slice(&f32::NAN.to_le_bytes());
        }
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let url = format!("sqlite:{}?mode=rw", cache_path.display());
            let pool = sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(1)
                .connect(&url)
                .await
                .unwrap();
            let res =
                sqlx::query("UPDATE embedding_cache SET embedding = ?1 WHERE content_hash = ?2")
                    .bind(&nan_blob)
                    .bind(&key)
                    .execute(&pool)
                    .await
                    .unwrap();
            assert_eq!(res.rows_affected(), 1, "corruption seed must hit the row");
            pool.close().await;
        });

        let split = resolve_reuse(&[chunk], &store, Some(&cache), dim, Some(fp)).unwrap();
        assert!(
            split.cached.is_empty(),
            "non-finite cache entry must not be served as a hit"
        );
        assert_eq!(
            split.to_embed,
            vec![0],
            "chunk with corrupt cache entry falls through to embedding"
        );
        assert_eq!(
            split.global_hits, 0,
            "dropped entry must not count as a global hit"
        );
    }

    /// Dim mismatch (model swap): the store cache is skipped, so a stored row at
    /// a different dim is NOT reused.
    #[test]
    fn dim_mismatch_skips_store_cache() {
        let store_dim = 8;
        let (_tmp, store) = open_store(store_dim);
        let mut seeded = chunk_with("seed", "fn a() {}", "k1");
        seeded.canonical_hash = "k1".to_string();
        let mut v = vec![0.0_f32; store_dim];
        v[0] = 1.0;
        store
            .upsert_chunks_batch(&[(seeded, Embedding::new(v))], Some(0))
            .unwrap();

        let lookup = chunk_with("a", "fn a() {}", "k1");
        // Resolve with a DIFFERENT embedder dim → store cache skipped.
        let split = resolve_reuse(&[lookup], &store, None, store_dim + 1, None).unwrap();
        assert!(
            split.cached.is_empty(),
            "dim mismatch must skip store cache"
        );
        assert_eq!(split.to_embed, vec![0]);
    }
}
