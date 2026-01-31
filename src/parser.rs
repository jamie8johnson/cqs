//! Code parsing with tree-sitter

use std::path::Path;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ParserError {
    #[error("Unsupported file type: {0}")]
    UnsupportedFileType(String),
    #[error("Failed to parse: {0}")]
    ParseFailed(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

pub struct Parser {
    // TODO: language configs
}

impl Parser {
    pub fn new() -> Result<Self, ParserError> {
        Ok(Self {})
    }

    pub fn parse_file(&self, _path: &Path) -> Result<Vec<Chunk>, ParserError> {
        todo!("implement parsing")
    }
}

impl Default for Parser {
    fn default() -> Self {
        Self::new().expect("failed to create parser")
    }
}

#[derive(Debug, Clone)]
pub struct Chunk {
    pub id: String,
    pub file: std::path::PathBuf,
    pub language: Language,
    pub chunk_type: ChunkType,
    pub name: String,
    pub signature: String,
    pub content: String,
    pub doc: Option<String>,
    pub line_start: u32,
    pub line_end: u32,
    pub content_hash: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Rust,
    Python,
    TypeScript,
    JavaScript,
    Go,
}

impl Language {
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext {
            "rs" => Some(Language::Rust),
            "py" | "pyi" => Some(Language::Python),
            "ts" | "tsx" => Some(Language::TypeScript),
            "js" | "jsx" | "mjs" | "cjs" => Some(Language::JavaScript),
            "go" => Some(Language::Go),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkType {
    Function,
    Method,
}
