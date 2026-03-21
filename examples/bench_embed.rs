use cqs::embedder::Embedder;
use std::time::Instant;

/// Entry point that benchmarks an embedder's initialization, warmup, and embedding performance.
///
/// Initializes an embedder instance, measures warmup time, then times individual query embeddings and batch document embeddings at different scales (10 and 50 documents). Prints timing results for each operation.
///
/// # Arguments
///
/// None
///
/// # Returns
///
/// None (returns unit type)
///
/// # Panics
///
/// Panics if embedder initialization, warmup, or any embedding operation fails (via `unwrap()` calls).
fn main() {
    println!("Initializing embedder...");
    let start = Instant::now();
    let embedder = Embedder::new().unwrap();
    println!("Init: {:?}", start.elapsed());
    println!("Provider: {}", embedder.provider());

    // Warmup
    println!("\nWarmup...");
    let start = Instant::now();
    embedder.warm().unwrap();
    println!("Warmup: {:?}", start.elapsed());

    // Single query embedding
    println!("\nSingle query embeddings:");
    for query in [
        "parse files",
        "database connection",
        "error handling",
        "semantic search",
        "tree sitter parsing",
    ] {
        let start = Instant::now();
        let _ = embedder.embed_query(query).unwrap();
        println!("  {:30} {:?}", query, start.elapsed());
    }

    // Batch embedding
    println!("\nBatch embedding (10 docs):");
    let docs: Vec<&str> = (0..10).map(|_| "fn example() { let x = 42; }").collect();
    let start = Instant::now();
    let _ = embedder.embed_documents(&docs).unwrap();
    println!(
        "  10 docs: {:?} ({:?}/doc)",
        start.elapsed(),
        start.elapsed() / 10
    );

    // Larger batch
    println!("\nBatch embedding (50 docs):");
    let docs: Vec<&str> = (0..50).map(|_| "fn example() { let x = 42; }").collect();
    let start = Instant::now();
    let _ = embedder.embed_documents(&docs).unwrap();
    println!(
        "  50 docs: {:?} ({:?}/doc)",
        start.elapsed(),
        start.elapsed() / 50
    );
}
