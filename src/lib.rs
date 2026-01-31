//! cq - semantic code search with local embeddings

pub mod embedder;
pub mod mcp;
pub mod parser;
pub mod store;

pub use embedder::Embedder;
pub use mcp::{serve_stdio, serve_http};
pub use parser::Parser;
pub use store::Store;
