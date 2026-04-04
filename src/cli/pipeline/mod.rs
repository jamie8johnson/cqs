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
use cqs::{Parser as CqParser, Store};

use embedding::{cpu_embed_stage, gpu_embed_stage};
use parsing::{parser_stage, ParserStageContext};
use types::{EmbeddedBatch, ParsedBatch, EMBED_CHANNEL_DEPTH, PARSE_CHANNEL_DEPTH};
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
        bounded(PARSE_CHANNEL_DEPTH);
    let (embed_tx, embed_rx): (Sender<EmbeddedBatch>, Receiver<EmbeddedBatch>) =
        bounded(EMBED_CHANNEL_DEPTH);
    let (fail_tx, fail_rx): (Sender<ParsedBatch>, Receiver<ParsedBatch>) =
        bounded(EMBED_CHANNEL_DEPTH);

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
                },
                parse_tx,
            )
        })
    };

    // Stage 2a: GPU embedder thread
    let gpu_model = model_config.clone();
    let gpu_handle = {
        let store = Arc::clone(&store);
        let embedded_count = Arc::clone(&embedded_count);
        let gpu_failures = Arc::clone(&gpu_failures);
        thread::spawn(move || {
            gpu_embed_stage(
                parse_rx,
                embed_tx,
                fail_tx,
                store,
                embedded_count,
                gpu_failures,
                gpu_model,
            )
        })
    };

    // Stage 2b: CPU embedder thread
    let cpu_model = model_config;
    let cpu_handle = {
        let store = Arc::clone(&store);
        let embedded_count = Arc::clone(&embedded_count);
        thread::spawn(move || {
            cpu_embed_stage(
                parse_rx_cpu,
                fail_rx,
                embed_tx_cpu,
                store,
                embedded_count,
                cpu_model,
            )
        })
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
                    tracing::warn!("Progress template error: {}, using default", e);
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

/// Extract a human-readable message from a thread panic payload.
fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
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
        assert_eq!(max_tokens_per_window(512), 480); // E5-base/BGE-large
        assert_eq!(max_tokens_per_window(8192), 8160); // nomic, jina
        assert_eq!(max_tokens_per_window(32768), 32736); // GTE-Qwen2
        assert_eq!(max_tokens_per_window(0), 480); // fallback
        assert!(max_tokens_per_window(64) >= 128); // floor

        // Overlap scales with window size
        assert_eq!(window_overlap_tokens(480), 64); // 512-token model: floor of 64
        assert_eq!(window_overlap_tokens(8160), 1020); // 8K model: ~12.5%
        assert_eq!(window_overlap_tokens(32736), 4092); // 32K model: ~12.5%
        assert!(window_overlap_tokens(0) >= 64); // always at least 64
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
        let embedder = Embedder::new_cpu(ModelConfig::resolve(None, None)).unwrap();

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

    // These tests must run sequentially — they mutate CQS_EMBED_BATCH_SIZE env var.
    // Combined into one test to avoid race conditions with parallel test execution.
    #[test]
    fn test_embed_batch_size() {
        use super::types::embed_batch_size;

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
