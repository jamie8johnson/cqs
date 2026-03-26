//! Chunk CRUD operations
//!
//! Split into submodules by concern:
//! - `crud` - upsert, metadata, delete, summaries
//! - `staleness` - prune, stale checks
//! - `embeddings` - embedding retrieval by hash
//! - `query` - chunk retrieval, search, identity, stats
//! - `async_helpers` - async fetch, batch insert, EmbeddingBatchIterator

mod async_helpers;
mod crud;
mod embeddings;
mod query;
mod staleness;

pub use staleness::PruneAllResult;

// Free async functions in async_helpers are pub(super) — accessible
// to sibling modules (crud.rs) via `super::async_helpers::`.

#[cfg(test)]
pub(super) mod test_utils {
    use crate::parser::{Chunk, ChunkType, Language};
    use std::path::PathBuf;

    /// Creates a mock Rust function chunk with generated content and hash.
    pub(super) fn make_chunk(name: &str, file: &str) -> Chunk {
        let content = format!("fn {}() {{ /* body */ }}", name);
        let hash = blake3::hash(content.as_bytes()).to_hex().to_string();
        Chunk {
            id: format!("{}:1:{}", file, &hash[..8]),
            file: PathBuf::from(file),
            language: Language::Rust,
            chunk_type: ChunkType::Function,
            name: name.to_string(),
            signature: format!("fn {}()", name),
            content,
            doc: None,
            line_start: 1,
            line_end: 5,
            content_hash: hash,
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
        }
    }
}
