//! Indexing pipeline for parsing, embedding, and storing code chunks
//!
//! Provides a 3-stage concurrent pipeline:
//! 1. Parser: Parse files in parallel batches
//! 2. Embedder: Embed chunks (GPU with CPU fallback)
//! 3. Writer: Write to SQLite

mod embedding;
mod parsing;
mod types;
mod upsert;
mod windowing;

// Re-export public items
pub(crate) use types::embed_batch_size_for;
pub(crate) use types::PipelineStats;
pub(crate) use windowing::apply_windowing;

use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;

use anyhow::{Context, Result};
use crossbeam_channel::{bounded, Receiver, Sender};
use indicatif::{ProgressBar, ProgressStyle};

use cqs::embedder::ModelConfig;
use cqs::{panic_message, Parser as CqParser, Store};

use embedding::{cpu_embed_stage, gpu_embed_stage};
use parsing::{parser_stage, ParserStageContext};
use types::{
    embed_channel_depth, parse_channel_depth, EmbedStageContext, EmbeddedBatch, ParsedBatch,
};
use upsert::store_stage;

/// Run the indexing pipeline with 3 concurrent stages:
/// 1. Parser: Parse files in parallel batches
/// 2. Embedder: Embed chunks (GPU with CPU fallback)
/// 3. Writer: Write to SQLite
pub(crate) fn run_index_pipeline(
    root: &Path,
    files: Vec<PathBuf>,
    store: Arc<Store>,
    force: bool,
    quiet: bool,
    model_config: ModelConfig,
) -> Result<PipelineStats> {
    let _span = tracing::info_span!("run_index_pipeline", file_count = files.len()).entered();
    let total_files = files.len();

    // Channels
    let (parse_tx, parse_rx): (Sender<ParsedBatch>, Receiver<ParsedBatch>) =
        bounded(parse_channel_depth());
    let (embed_tx, embed_rx): (Sender<EmbeddedBatch>, Receiver<EmbeddedBatch>) =
        bounded(embed_channel_depth());
    let (fail_tx, fail_rx): (Sender<ParsedBatch>, Receiver<ParsedBatch>) =
        bounded(embed_channel_depth());

    // Shared state
    let parser = Arc::new(CqParser::new().context("Failed to initialize parser")?);
    let parsed_count = Arc::new(AtomicUsize::new(0));
    let embedded_count = Arc::new(AtomicUsize::new(0));
    let gpu_failures = Arc::new(AtomicUsize::new(0));
    let parse_errors = Arc::new(AtomicUsize::new(0));

    // CPU embedder also races on parse_rx
    let parse_rx_cpu = parse_rx.clone();
    let embed_tx_cpu = embed_tx.clone();

    // Stage 1: Parser thread
    let parser_handle = {
        let parser = Arc::clone(&parser);
        let store = Arc::clone(&store);
        let parsed_count = Arc::clone(&parsed_count);
        let parse_errors = Arc::clone(&parse_errors);
        let root = root.to_path_buf();
        let model_config = model_config.clone();
        thread::spawn(move || {
            parser_stage(
                files,
                ParserStageContext {
                    root,
                    force,
                    parser,
                    store,
                    parsed_count,
                    parse_errors,
                    model_config,
                },
                parse_tx,
            )
        })
    };

    // Open project-scoped embeddings cache (best-effort).
    //
    // Spec §Cache: cache lives at `<project_cqs_dir>/embeddings_cache.db`,
    // shared across all slots so an embedder swap only re-embeds chunks
    // whose hash hasn't been seen for that model_id before.
    //
    // `CQS_CACHE_ENABLED=0` disables the cache entirely for benchmarking /
    // debugging — the embed path falls back to per-batch GPU/CPU work without
    // the partition step.
    let global_cache: Option<Arc<cqs::cache::EmbeddingCache>> = {
        if std::env::var("CQS_CACHE_ENABLED").as_deref() == Ok("0") {
            tracing::info!("CQS_CACHE_ENABLED=0 — embeddings cache disabled for this run");
            None
        } else {
            let project_cqs_dir = cqs::resolve_index_dir(root);
            let cache_path = cqs::cache::EmbeddingCache::project_default_path(&project_cqs_dir);
            match cqs::cache::EmbeddingCache::open(&cache_path) {
                Ok(c) => {
                    tracing::info!(
                        path = %cache_path.display(),
                        "Project embeddings cache opened"
                    );
                    Some(Arc::new(c))
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        path = %cache_path.display(),
                        "Project embeddings cache unavailable; disabling cache for this run"
                    );
                    None
                }
            }
        }
    };

    // Stage 2a: GPU embedder thread
    let gpu_handle = {
        let ctx = EmbedStageContext {
            store: Arc::clone(&store),
            embedded_count: Arc::clone(&embedded_count),
            model_config: model_config.clone(),
            global_cache: global_cache.clone(),
        };
        let gpu_failures = Arc::clone(&gpu_failures);
        thread::spawn(move || gpu_embed_stage(parse_rx, embed_tx, fail_tx, ctx, gpu_failures))
    };

    // Stage 2b: CPU embedder thread
    let cpu_handle = {
        let ctx = EmbedStageContext {
            store: Arc::clone(&store),
            embedded_count: Arc::clone(&embedded_count),
            model_config,
            global_cache: global_cache.clone(),
        };
        thread::spawn(move || cpu_embed_stage(parse_rx_cpu, fail_rx, embed_tx_cpu, ctx))
    };

    // Stage 3: Writer (main thread)
    let progress = if quiet {
        ProgressBar::hidden()
    } else {
        let pb = ProgressBar::new(total_files as u64);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("[{elapsed_precise}] {bar:40.cyan/blue} {msg}")
                .unwrap_or_else(|e| {
                    // P3 #95: structured fields — same rationale as the
                    // sibling parse warn in `pipeline/parsing.rs`.
                    tracing::warn!(error = %e, "Progress bar template invalid, using default");
                    ProgressStyle::default_bar()
                }),
        );
        pb
    };

    let (total_embedded, total_cached, total_type_edges, total_calls) =
        store_stage(embed_rx, &store, &parsed_count, &embedded_count, &progress)?;

    progress.finish_with_message("done");

    // Wait for threads to finish
    parser_handle
        .join()
        .map_err(|e| anyhow::anyhow!("Parser thread panicked: {}", panic_message(&e)))??;
    gpu_handle
        .join()
        .map_err(|e| anyhow::anyhow!("GPU embedder thread panicked: {}", panic_message(&e)))??;
    cpu_handle
        .join()
        .map_err(|e| anyhow::anyhow!("CPU embedder thread panicked: {}", panic_message(&e)))??;

    // Evict global cache if over size limit
    if let Some(ref cache) = global_cache {
        if let Err(e) = cache.evict() {
            tracing::warn!(error = %e, "Global cache eviction failed");
        }
    }

    // Update the "updated_at" metadata timestamp
    if let Err(e) = store.touch_updated_at() {
        tracing::warn!(error = %e, "Failed to update timestamp");
    }

    let stats = PipelineStats {
        total_embedded,
        total_cached,
        gpu_failures: gpu_failures.load(Ordering::Relaxed),
        parse_errors: parse_errors.load(Ordering::Relaxed),
        total_type_edges,
        total_calls,
    };

    tracing::info!(
        total_embedded = stats.total_embedded,
        total_cached = stats.total_cached,
        gpu_failures = stats.gpu_failures,
        parse_errors = stats.parse_errors,
        total_type_edges = stats.total_type_edges,
        total_calls = stats.total_calls,
        "Pipeline indexing complete"
    );

    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::embedding::create_embedded_batch;
    use super::types::RelationshipData;
    use super::windowing::*;
    use cqs::language::{ChunkType, Language};
    use cqs::{Chunk, Embedding};
    use std::path::PathBuf;

    /// Creates a test Chunk with minimal configuration for testing purposes.
    ///
    /// # Arguments
    ///
    /// * `id` - A string identifier for the chunk, used as both the chunk ID and name
    /// * `content` - The source code content to be stored in the chunk
    ///
    /// # Returns
    ///
    /// A new `Chunk` instance with:
    /// - File path set to "test.rs"
    /// - Language set to Rust
    /// - Chunk type set to Function
    /// - Content hash computed from the provided content
    /// - Line range from 1 to 10
    /// - All optional fields set to None or empty
    fn make_test_chunk(id: &str, content: &str) -> Chunk {
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
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        }
    }

    fn test_mtimes(mtime: i64) -> std::collections::HashMap<PathBuf, i64> {
        let mut m = std::collections::HashMap::new();
        m.insert(PathBuf::from("test.rs"), mtime);
        m
    }

    #[test]
    fn test_create_embedded_batch_all_cached() {
        let chunk = make_test_chunk("c1", "fn foo() {}");
        let emb = Embedding::new(vec![0.0; cqs::EMBEDDING_DIM]);
        let cached = vec![(chunk, emb)];

        let batch = create_embedded_batch(
            cached,
            vec![],
            vec![],
            RelationshipData::default(),
            test_mtimes(12345),
        );
        assert_eq!(batch.chunk_embeddings.len(), 1);
        assert_eq!(batch.cached_count, 1);
        assert_eq!(batch.file_mtimes[&PathBuf::from("test.rs")], 12345);
    }

    #[test]
    fn test_create_embedded_batch_all_new() {
        let chunk = make_test_chunk("c1", "fn foo() {}");
        let emb = Embedding::new(vec![1.0; cqs::EMBEDDING_DIM]);

        let batch = create_embedded_batch(
            vec![],
            vec![chunk],
            vec![emb],
            RelationshipData::default(),
            test_mtimes(99),
        );
        assert_eq!(batch.chunk_embeddings.len(), 1);
        assert_eq!(batch.cached_count, 0);
        assert_eq!(batch.file_mtimes[&PathBuf::from("test.rs")], 99);
    }

    #[test]
    fn test_create_embedded_batch_mixed() {
        let cached_chunk = make_test_chunk("c1", "fn foo() {}");
        let cached_emb = Embedding::new(vec![0.0; cqs::EMBEDDING_DIM]);
        let new_chunk = make_test_chunk("c2", "fn bar() {}");
        let new_emb = Embedding::new(vec![1.0; cqs::EMBEDDING_DIM]);

        let batch = create_embedded_batch(
            vec![(cached_chunk, cached_emb)],
            vec![new_chunk],
            vec![new_emb],
            RelationshipData::default(),
            test_mtimes(12345),
        );
        assert_eq!(batch.chunk_embeddings.len(), 2);
        assert_eq!(batch.cached_count, 1);
    }

    #[test]
    fn test_create_embedded_batch_empty() {
        let batch = create_embedded_batch(
            vec![],
            vec![],
            vec![],
            RelationshipData::default(),
            std::collections::HashMap::new(),
        );
        assert_eq!(batch.chunk_embeddings.len(), 0);
        assert_eq!(batch.cached_count, 0);
    }

    #[test]
    fn test_create_embedded_batch_preserves_order() {
        let c1 = make_test_chunk("c1", "fn first() {}");
        let e1 = Embedding::new(vec![1.0; cqs::EMBEDDING_DIM]);
        let c2 = make_test_chunk("c2", "fn second() {}");
        let e2 = Embedding::new(vec![2.0; cqs::EMBEDDING_DIM]);
        let c3 = make_test_chunk("c3", "fn third() {}");
        let e3 = Embedding::new(vec![3.0; cqs::EMBEDDING_DIM]);

        let batch = create_embedded_batch(
            vec![(c1, e1)],
            vec![c2, c3],
            vec![e2, e3],
            RelationshipData::default(),
            test_mtimes(0),
        );

        assert_eq!(batch.chunk_embeddings.len(), 3);
        // Cached come first, then new in order
        assert_eq!(batch.chunk_embeddings[0].0.id, "c1");
        assert_eq!(batch.chunk_embeddings[1].0.id, "c2");
        assert_eq!(batch.chunk_embeddings[2].0.id, "c3");
    }

    #[test]
    fn test_windowing_constants() {
        // Verify windowing function produces sensible values
        // Short prefix (E5 "passage: " = 3 tokens, BGE "" = 0): overhead is dominated
        // by SPECIAL_TOKEN_OVERHEAD = 4.
        assert_eq!(max_tokens_per_window(512, 3), 505); // E5-base: 512 - (3 + 4)
        assert_eq!(max_tokens_per_window(512, 0), 508); // BGE-large: 512 - (0 + 4)
        assert_eq!(max_tokens_per_window(8192, 3), 8185); // nomic-style 8K, short prefix
        assert_eq!(max_tokens_per_window(32768, 3), 32761); // GTE-Qwen2

        // #1042: long-prefix instruction model (nomic-embed-code: ~38-token prefix)
        // shrinks the window so prefix + window + special tokens stay under max_seq.
        assert_eq!(max_tokens_per_window(512, 38), 470);
        assert_eq!(max_tokens_per_window(8192, 38), 8150);

        assert_eq!(max_tokens_per_window(0, 3), 480); // fallback
        assert!(max_tokens_per_window(64, 0) >= 128); // floor

        // Overlap scales with window size, clamped below max_tokens/2
        assert_eq!(window_overlap_tokens(480), 64); // 512-token model: floor of 64
        assert_eq!(window_overlap_tokens(8160), 1020); // 8K model: ~12.5%
        assert_eq!(window_overlap_tokens(32736), 4092); // 32K model: ~12.5%
        assert_eq!(window_overlap_tokens(0), 0); // degenerate: no tokens, no overlap

        // AC-8: overlap must stay below max_tokens/2 for split_into_windows
        assert_eq!(window_overlap_tokens(128), 63); // min window from max_tokens_per_window
        assert!(window_overlap_tokens(128) < 128 / 2);
        assert!(window_overlap_tokens(200) < 200 / 2);
    }

    #[test]
    #[ignore] // Requires model
    fn test_apply_windowing_empty() {
        use cqs::embedder::ModelConfig;
        use cqs::Embedder;
        let embedder = Embedder::new_cpu(ModelConfig::resolve(None, None)).unwrap();
        let result = apply_windowing(vec![], &embedder);
        assert!(result.is_empty());
    }

    #[test]
    #[ignore] // Requires model
    fn test_apply_windowing_short_chunk() {
        use cqs::embedder::ModelConfig;
        use cqs::Embedder;
        let embedder = Embedder::new_cpu(ModelConfig::resolve(None, None)).unwrap();
        let mut chunk = make_test_chunk("short1", "fn foo() {}");
        chunk.doc = Some("A short function".to_string());

        let result = apply_windowing(vec![chunk], &embedder);

        assert_eq!(result.len(), 1);
        let c = &result[0];
        assert_eq!(c.id, "short1");
        assert_eq!(c.name, "short1");
        assert_eq!(c.doc, Some("A short function".to_string()));
        assert_eq!(c.parent_id, None, "short chunk should not have parent_id");
        assert_eq!(c.window_idx, None, "short chunk should not have window_idx");
        assert_eq!(c.file, PathBuf::from("test.rs"));
        assert_eq!(c.language, Language::Rust);
        assert_eq!(c.chunk_type, ChunkType::Function);
        assert_eq!(c.content, "fn foo() {}");
    }

    #[test]
    #[ignore] // Requires model
    fn test_apply_windowing_long_chunk() {
        use cqs::embedder::ModelConfig;
        use cqs::Embedder;
        let embedder = match Embedder::new_cpu(ModelConfig::resolve(None, None)) {
            Ok(e) => e,
            Err(err) => {
                eprintln!("CPU embedder unavailable in test env: {err}; skipping (#1305)");
                return;
            }
        };

        // Probe the tokenizer health up front. `apply_windowing` swallows
        // tokenize errors and passes the chunk through unchanged — fine
        // for production (best-effort fallback) but it'd make the
        // `result.len() > 1` assertion below fire with a "got 1" misleading
        // diagnostic when the real cause is a corrupt tokenizer.json (e.g.
        // CI runner half-populated HF cache, see #1305). Skip cleanly
        // instead.
        if let Err(err) = embedder.token_count("probe") {
            eprintln!("tokenizer unhealthy in test env: {err}; skipping (#1305)");
            return;
        }

        // Build content that exceeds 480 tokens. Each line is a unique function body.
        // ~500 lines of "let varN = N;\n" should comfortably exceed the token limit.
        let long_content: String = (0..500)
            .map(|i| format!("    let variable_{i} = {i};\n"))
            .collect();
        let content = format!("fn big_function() {{\n{long_content}}}");

        let mut chunk = make_test_chunk("long1", &content);
        chunk.doc = Some("A very long function".to_string());
        chunk.line_start = 10;
        chunk.line_end = 520;
        chunk.parent_type_name = Some("MyStruct".to_string());

        let original_id = chunk.id.clone();
        let result = apply_windowing(vec![chunk], &embedder);

        assert!(
            result.len() > 1,
            "Expected multiple windows, got {}",
            result.len()
        );

        for (i, window) in result.iter().enumerate() {
            let idx = i as u32;

            // ID format: "{parent_id}:w{idx}"
            assert_eq!(
                window.id,
                format!("{original_id}:w{idx}"),
                "window {i} has wrong id"
            );

            // parent_id set on all windows
            assert_eq!(
                window.parent_id,
                Some(original_id.clone()),
                "window {i} missing parent_id"
            );

            // window_idx set correctly
            assert_eq!(
                window.window_idx,
                Some(idx),
                "window {i} has wrong window_idx"
            );

            // Shared fields from parent
            assert_eq!(window.file, PathBuf::from("test.rs"));
            assert_eq!(window.language, Language::Rust);
            assert_eq!(window.chunk_type, ChunkType::Function);
            assert_eq!(window.name, "long1");
            assert_eq!(window.line_start, 10);
            assert_eq!(window.line_end, 520);
            assert_eq!(window.parent_type_name, Some("MyStruct".to_string()));

            // Content hash is blake3 of the window content
            let expected_hash = blake3::hash(window.content.as_bytes()).to_hex().to_string();
            assert_eq!(
                window.content_hash, expected_hash,
                "window {i} hash mismatch"
            );

            // Non-empty content
            assert!(!window.content.is_empty(), "window {i} has empty content");
        }

        // First window gets doc, subsequent windows do not
        assert_eq!(
            result[0].doc,
            Some("A very long function".to_string()),
            "first window should preserve doc"
        );
        for window in &result[1..] {
            assert_eq!(window.doc, None, "non-first window should have doc = None");
        }
    }

    #[test]
    fn test_embed_batch_size() {
        use super::types::{embed_batch_size, TEST_ENV_MUTEX};
        let _lock = TEST_ENV_MUTEX.lock().unwrap_or_else(|p| p.into_inner());

        // Default
        std::env::remove_var("CQS_EMBED_BATCH_SIZE");
        assert_eq!(embed_batch_size(), 64);

        // Override
        std::env::set_var("CQS_EMBED_BATCH_SIZE", "128");
        assert_eq!(embed_batch_size(), 128);
        std::env::remove_var("CQS_EMBED_BATCH_SIZE");

        // Invalid falls back to default
        std::env::set_var("CQS_EMBED_BATCH_SIZE", "not_a_number");
        assert_eq!(embed_batch_size(), 64);
        std::env::remove_var("CQS_EMBED_BATCH_SIZE");

        // Zero falls back to default
        std::env::set_var("CQS_EMBED_BATCH_SIZE", "0");
        assert_eq!(embed_batch_size(), 64);
        std::env::remove_var("CQS_EMBED_BATCH_SIZE");
    }
}
