//! SQLite storage for chunks and embeddings

use std::path::Path;
use thiserror::Error;

use crate::embedder::Embedding;
use crate::parser::Chunk;

#[derive(Error, Debug)]
pub enum StoreError {
    #[error("Database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

pub struct Store {
    // TODO: rusqlite connection
}

impl Store {
    pub fn open(_path: &Path) -> Result<Self, StoreError> {
        todo!("implement store open")
    }

    pub fn upsert_chunk(&self, _chunk: &Chunk, _embedding: &Embedding) -> Result<(), StoreError> {
        todo!("implement upsert")
    }

    pub fn search(
        &self,
        _query: &Embedding,
        _limit: usize,
        _threshold: f32,
    ) -> Result<Vec<SearchResult>, StoreError> {
        todo!("implement search")
    }
}

#[derive(Debug)]
pub struct SearchResult {
    pub chunk: ChunkSummary,
    pub score: f32,
}

#[derive(Debug)]
pub struct ChunkSummary {
    pub id: String,
    pub file: std::path::PathBuf,
    pub name: String,
    pub signature: String,
    pub line_start: u32,
}
