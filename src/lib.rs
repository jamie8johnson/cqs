//! # cqs - Semantic Code Search
//!
//! Local semantic search for code using ML embeddings.
//! Find functions by what they do, not just their names.
//!
//! ## Features
//!
//! - **Semantic search**: Uses E5-base-v2 embeddings (769-dim: 768 model + sentiment)
//! - **Notes with sentiment**: Unified memory system for AI collaborators
//! - **Multi-language**: Rust, Python, TypeScript, JavaScript, Go
//! - **GPU acceleration**: CUDA/TensorRT with CPU fallback
//! - **MCP integration**: Works with Claude Code and other AI assistants
//!
//! ## Quick Start
//!
//! ```no_run
//! use cqs::{Embedder, Parser, Store};
//! use cqs::store::ModelInfo;
//!
//! # fn main() -> anyhow::Result<()> {
//! // Initialize components
//! let parser = Parser::new()?;
//! let mut embedder = Embedder::new()?;
//! let store = Store::open(std::path::Path::new(".cq/index.db"))?;
//!
//! // Parse and embed a file
//! let chunks = parser.parse_file(std::path::Path::new("src/main.rs"))?;
//! let embeddings = embedder.embed_documents(
//!     &chunks.iter().map(|c| c.content.as_str()).collect::<Vec<_>>()
//! )?;
//!
//! // Search for similar code
//! let query_embedding = embedder.embed_query("parse configuration file")?;
//! let results = store.search(&query_embedding, 5, 0.3)?;
//! # Ok(())
//! # }
//! ```
//!
//! ## MCP Server
//!
//! Start the MCP server for AI assistant integration:
//!
//! ```no_run
//! # fn example() -> anyhow::Result<()> {
//! // Stdio transport (for Claude Code)
//! cqs::serve_stdio(".".into(), false)?;  // false = CPU, true = GPU
//!
//! // HTTP transport with GPU embedding (None = no auth)
//! // Note: serve_http blocks the current thread
//! cqs::serve_http(".".into(), "127.0.0.1", 3000, true, None)?;
//! # Ok(())
//! # }
//! ```

pub mod config;
pub mod embedder;
pub mod hnsw;
pub mod index;
pub mod language;
pub mod mcp;
pub mod nl;
pub mod note;
pub mod parser;
pub mod search;
pub mod source;
pub mod store;

#[cfg(feature = "gpu-search")]
pub mod cagra;

pub use embedder::Embedder;
pub use hnsw::HnswIndex;
pub use index::{IndexResult, VectorIndex};
pub use mcp::{serve_http, serve_stdio};
pub use parser::Parser;
pub use store::Store;

#[cfg(feature = "gpu-search")]
pub use cagra::CagraIndex;
