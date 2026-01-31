//! cq - semantic code search with local embeddings

pub mod embedder;
pub mod mcp;
pub mod parser;
pub mod store;

pub use embedder::Embedder;
pub use mcp::{serve_http, serve_stdio};
pub use parser::Parser;
pub use store::Store;
