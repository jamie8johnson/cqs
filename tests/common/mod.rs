// Helpers in this module are used by different subsets of test binaries.
// Each binary that does `mod common;` only references some of the items,
// so per-item `#[allow(dead_code)]` becomes a maintenance treadmill.
// File-level allow is the right call.
#![allow(dead_code, clippy::type_complexity)]

//! Common test fixtures and helpers
//!
//! Two fixture tiers:
//!
//! - [`TestStore`] — bare `Store` + tempdir. Use when the test only
//!   exercises the storage layer (chunk CRUD, FTS, call graph rows,
//!   etc.) and you don't need a parser or embedder.
//!
//! - [`InProcessFixture`] — `TestStore` + [`Parser`] + an embedder.
//!   Use when the test needs to "act like the indexer": parse source
//!   files, produce embeddings, upsert them, then assert. Replaces the
//!   subprocess pattern used by the gated `slow-tests` binaries — see
//!   `docs/plans/2026-04-22-cqs-slow-tests-elimination.md`.
//!
//!   By default the fixture uses [`MockEmbedder`], which hashes content
//!   into deterministic vectors so retrieval works without loading any
//!   ML model (~ms per test). For the handful of tests that need *real*
//!   semantic behaviour, `with_real_embedder()` swaps in a shared,
//!   lazily-cold-loaded `cqs::Embedder` (one per test binary).
//!
//! Usage in test files:
//! ```ignore
//! mod common;
//! use common::{InProcessFixture, TestStore};
//! ```

use cqs::embedder::{Embedder, Embedding, ModelConfig};
use cqs::parser::{Chunk, ChunkType, Language, Parser};
use cqs::store::{ModelInfo, Store};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use tempfile::TempDir;

/// Test store with automatic cleanup
///
/// Wraps a `Store` with its backing `TempDir`, ensuring the directory
/// lives as long as the store is in use.
pub struct TestStore {
    /// The store instance
    pub store: Store,
    /// Temp directory (kept alive to prevent cleanup)
    _dir: TempDir,
}

impl TestStore {
    /// Create an initialized test store in a temporary directory
    #[allow(dead_code)]
    pub fn new() -> Self {
        let dir = TempDir::new().expect("Failed to create temp dir");
        let db_path = dir.path().join(cqs::INDEX_DB_FILENAME);
        let store = Store::open(&db_path).expect("Failed to open store");
        store
            .init(&ModelInfo::default())
            .expect("Failed to init store");
        Self { store, _dir: dir }
    }

    /// Get the database path for this test store
    #[allow(dead_code)]
    pub fn db_path(&self) -> PathBuf {
        self._dir.path().join(cqs::INDEX_DB_FILENAME)
    }

    /// Create a test store with custom model info
    #[allow(dead_code)]
    pub fn with_model(model: &ModelInfo) -> Self {
        let dir = TempDir::new().expect("Failed to create temp dir");
        let db_path = dir.path().join(cqs::INDEX_DB_FILENAME);
        let store = Store::open(&db_path).expect("Failed to open store");
        store.init(model).expect("Failed to init store");
        Self { store, _dir: dir }
    }
}

impl std::ops::Deref for TestStore {
    type Target = Store;

    fn deref(&self) -> &Self::Target {
        &self.store
    }
}

/// Create a test chunk with sensible defaults
#[allow(dead_code)]
pub fn test_chunk(name: &str, content: &str) -> Chunk {
    let hash = blake3::hash(content.as_bytes()).to_hex().to_string();
    Chunk {
        id: format!("test.rs:1:{}", &hash[..8]),
        file: PathBuf::from("test.rs"),
        language: Language::Rust,
        chunk_type: ChunkType::Function,
        name: name.to_string(),
        signature: format!("fn {}()", name),
        content: content.to_string(),
        doc: None,
        line_start: 1,
        line_end: 5,
        content_hash: hash,
        parent_id: None,
        window_idx: None,
        parent_type_name: None,
        parser_version: 0,
    }
}

/// Create a mock embedding from a scalar seed.
///
/// The seed value determines the direction of the embedding vector.
/// Same seed = same direction = high similarity.
/// Different seeds = different directions = lower similarity.
///
/// Dimension matches `cqs::EMBEDDING_DIM` (1024 with the default BGE-large
/// preset).
pub fn mock_embedding(seed: f32) -> Embedding {
    let mut v = vec![seed; cqs::EMBEDDING_DIM];
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in &mut v {
            *x /= norm;
        }
    }
    Embedding::new(v)
}

/// Content-deterministic mock embedding: same text → same vector.
///
/// Built by spreading a blake3 digest of `text` across the 1024
/// dimensions. Useful for harness tests where we want
/// `embed("foo")` and `embed("foo")` to match (so that `search("foo")`
/// returns the chunk that contains "foo") *without* loading an ONNX
/// model. Doesn't preserve real semantic similarity — synonyms won't
/// cluster — so tests that rely on semantic behaviour should use
/// `InProcessFixture::with_real_embedder()` instead.
pub fn mock_embed_text(text: &str) -> Embedding {
    let hash = blake3::hash(text.as_bytes());
    let bytes = hash.as_bytes(); // 32 bytes
    let mut v = vec![0f32; cqs::EMBEDDING_DIM];
    for (i, slot) in v.iter_mut().enumerate() {
        let byte = bytes[i % bytes.len()];
        // Map [0, 256) → [-1, 1] then scatter slightly per-dim so two
        // strings differing in one char don't collapse onto each other
        // after normalisation.
        let centred = (byte as f32 - 128.0) / 128.0;
        *slot = centred + (i as f32) * 1e-4 * centred.signum();
    }
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in &mut v {
            *x /= norm;
        }
    }
    Embedding::new(v)
}

/// Trait abstracting the production [`Embedder`] so tests can swap in
/// a deterministic mock without paying the model cold-load cost.
///
/// Only the two methods cqs's indexer and search paths actually call
/// are required; everything else can stay on the concrete `Embedder`.
pub trait TestEmbedder: Send + Sync {
    fn embed_documents(&self, texts: &[&str]) -> Vec<Embedding>;
    fn embed_query(&self, text: &str) -> Embedding;
}

/// Mock embedder built on [`mock_embed_text`]. Zero model load,
/// deterministic, sub-microsecond per embedding.
pub struct MockEmbedder;

impl TestEmbedder for MockEmbedder {
    fn embed_documents(&self, texts: &[&str]) -> Vec<Embedding> {
        texts.iter().map(|t| mock_embed_text(t)).collect()
    }
    fn embed_query(&self, text: &str) -> Embedding {
        mock_embed_text(text)
    }
}

/// Adapter so the production [`cqs::Embedder`] satisfies [`TestEmbedder`].
/// The first call lazy-loads the ONNX session.
impl TestEmbedder for Embedder {
    fn embed_documents(&self, texts: &[&str]) -> Vec<Embedding> {
        Embedder::embed_documents(self, texts).expect("real embedder embed_documents failed")
    }
    fn embed_query(&self, text: &str) -> Embedding {
        Embedder::embed_query(self, text).expect("real embedder embed_query failed")
    }
}

/// Shared real embedder, lazy-loaded once per test binary. Cold-load
/// happens on first `with_real_embedder()` call; subsequent calls
/// reuse the same `Arc` — saves ~2-5s per test on the binaries that
/// genuinely need semantic embeddings.
fn shared_real_embedder() -> Arc<Embedder> {
    static REAL: OnceLock<Arc<Embedder>> = OnceLock::new();
    REAL.get_or_init(|| {
        let cfg = ModelConfig::resolve(None, None);
        Arc::new(Embedder::new_cpu(cfg).expect("failed to construct shared test Embedder"))
    })
    .clone()
}

/// In-process integration fixture: store + parser + embedder, all
/// living in a single tempdir. Replaces the subprocess pattern used
/// by the gated `slow-tests` binaries.
///
/// Lifetime: drop the fixture and the tempdir is reaped (with the
/// `.cqs/` index inside it). The shared real-embedder is *not*
/// dropped — it's pinned by [`shared_real_embedder`]'s `OnceLock`
/// for the lifetime of the test binary.
pub struct InProcessFixture {
    /// The store fixture (gives us `Store` + tempdir cleanup).
    pub store: TestStore,
    /// Configured tree-sitter parser. Cheap to construct; not shared
    /// between fixtures because `Parser` isn't `Sync` in all configs.
    pub parser: Parser,
    /// Embedder behind the test trait — `MockEmbedder` by default,
    /// real `cqs::Embedder` when constructed via `with_real_embedder()`.
    pub embedder: Arc<dyn TestEmbedder>,
    /// "Project root" inside the tempdir where source files live.
    /// `with_corpus()` writes under `root/src/`; `write_file()` accepts
    /// a name relative to `root/`.
    pub root: PathBuf,
}

impl InProcessFixture {
    /// Build a fixture with the [`MockEmbedder`]. No source files, no
    /// indexed chunks — call `with_corpus`/`write_file` to populate.
    pub fn new() -> Self {
        let store = TestStore::new();
        let root = store.db_path().parent().unwrap().to_path_buf();
        let parser = Parser::new().expect("failed to construct test Parser");
        Self {
            store,
            parser,
            embedder: Arc::new(MockEmbedder),
            root,
        }
    }

    /// Build a fixture with the real shared `cqs::Embedder`. First
    /// call per test binary cold-loads the ONNX model (~2-5s); later
    /// calls reuse it. Use only for tests that depend on real semantic
    /// similarity — most tests do fine with the mock.
    pub fn with_real_embedder() -> Self {
        let mut f = Self::new();
        f.embedder = shared_real_embedder();
        f
    }

    /// Convenience: build a fixture pre-populated with a sample
    /// corpus. Each entry is `(relative_path_under_root, content)`.
    /// The fixture writes the files, parses them, embeds the chunks,
    /// and upserts into the store before returning.
    pub fn with_corpus(files: &[(&str, &str)]) -> Self {
        let mut f = Self::new();
        for (path, content) in files {
            f.write_file(path, content)
                .expect("seed corpus write_file failed");
        }
        f.index().expect("seed corpus index failed");
        f
    }

    /// Write a single file to the fixture root, creating parent
    /// directories as needed.
    pub fn write_file(&self, rel_path: &str, content: &str) -> std::io::Result<()> {
        let p = self.root.join(rel_path);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(p, content)
    }

    /// Re-index every file currently under `root/` (recursively),
    /// using the configured embedder. Mirrors what `cqs index` does
    /// at the library level: parse → embed → upsert.
    ///
    /// `enumerate_files` returns paths relative to the root, so we join
    /// each with `self.root` before handing it to the parser — without
    /// that join the parser would resolve `src/lib.rs` against cargo's
    /// CWD (the cqs project itself) and end up parsing cqs's own code
    /// into the test store. Found that the hard way during phase 1
    /// harness validation.
    pub fn index(&mut self) -> Result<usize, Box<dyn std::error::Error>> {
        let exts = self.parser.supported_extensions();
        let files = cqs::enumerate_files(&self.root, &exts, false)?;
        let mut total = 0usize;
        let mut all_calls: Vec<(PathBuf, Vec<cqs::parser::FunctionCalls>)> = Vec::new();
        let mut all_types: Vec<(PathBuf, Vec<cqs::parser::ChunkTypeRefs>)> = Vec::new();
        for rel_path in files {
            let abs_path = self.root.join(&rel_path);
            // parse_file_all returns chunks + function_calls + type_refs in
            // a single tree-sitter walk. Tests for cqs::health, cqs::suggest,
            // deps, callers/callees etc. expect those tables to be populated
            // when the source has the relationships, so we pay the small
            // extra cost here instead of in each test.
            let (chunks, calls, type_refs) = match self.parser.parse_file_all(&abs_path) {
                Ok(parts) => parts,
                Err(_) => continue,
            };
            if !chunks.is_empty() {
                let texts: Vec<&str> = chunks.iter().map(|c| c.content.as_str()).collect();
                let embeddings = self.embedder.embed_documents(&texts);
                let pairs: Vec<(Chunk, Embedding)> = chunks.into_iter().zip(embeddings).collect();
                total += self.store.upsert_chunks_batch(&pairs, None)?;
            }
            if !calls.is_empty() {
                all_calls.push((abs_path.clone(), calls));
            }
            if !type_refs.is_empty() {
                all_types.push((abs_path, type_refs));
            }
        }
        if !all_calls.is_empty() {
            self.store.upsert_function_calls_for_files(&all_calls)?;
        }
        if !all_types.is_empty() {
            self.store.upsert_type_edges_for_files(&all_types)?;
        }
        Ok(total)
    }

    /// Convenience: embed `query` via the configured embedder, then
    /// search the store. Returns the raw `SearchResult`s; tests assert
    /// on names / scores / counts as needed.
    pub fn search(
        &self,
        query: &str,
        n: usize,
    ) -> Result<Vec<cqs::store::SearchResult>, Box<dyn std::error::Error>> {
        let q_emb = self.embedder.embed_query(query);
        let filter = cqs::store::SearchFilter::default();
        let results = self.store.search_filtered(&q_emb, &filter, n, 0.0)?;
        Ok(results)
    }

    /// Project root path (the parent of `.cqs/`).
    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl Default for InProcessFixture {
    fn default() -> Self {
        Self::new()
    }
}
