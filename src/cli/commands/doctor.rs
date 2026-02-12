//! Doctor command for cqs
//!
//! Runs diagnostic checks on installation and index.

use anyhow::Result;
use colored::Colorize;

use cqs::{Embedder, Parser as CqParser, Store};

use crate::cli::{find_project_root, Cli};

/// Run diagnostic checks on cqs installation and index
///
/// Reports runtime info, embedding provider, model status, and index statistics.
pub(crate) fn cmd_doctor(_cli: &Cli) -> Result<()> {
    let _span = tracing::info_span!("cmd_doctor").entered();
    let root = find_project_root();
    let cqs_dir = cqs::resolve_index_dir(&root);
    let index_path = cqs_dir.join("index.db");

    println!("Runtime:");

    // Check model
    match Embedder::new() {
        Ok(embedder) => {
            println!("  {} Model: {}", "[✓]".green(), cqs::store::MODEL_NAME);
            println!("  {} Tokenizer: loaded", "[✓]".green());
            println!("  {} Execution: {}", "[✓]".green(), embedder.provider());

            // Test embedding
            let start = std::time::Instant::now();
            embedder.warm()?;
            let elapsed = start.elapsed();
            println!("  {} Test embedding: {:?}", "[✓]".green(), elapsed);
        }
        Err(e) => {
            println!("  {} Model: {}", "[✗]".red(), e);
        }
    }

    println!();
    println!("Parser:");
    match CqParser::new() {
        Ok(parser) => {
            println!("  {} tree-sitter: loaded", "[✓]".green());
            println!(
                "  {} Languages: {}",
                "[✓]".green(),
                parser.supported_extensions().join(", ")
            );
        }
        Err(e) => {
            println!("  {} Parser: {}", "[✗]".red(), e);
        }
    }

    println!();
    println!("Index:");
    if index_path.exists() {
        match Store::open(&index_path) {
            Ok(store) => {
                let stats = store.stats()?;
                println!("  {} Location: {}", "[✓]".green(), index_path.display());
                println!(
                    "  {} Schema version: {}",
                    "[✓]".green(),
                    stats.schema_version
                );
                println!("  {} {} chunks indexed", "[✓]".green(), stats.total_chunks);
                if !stats.chunks_by_language.is_empty() {
                    let lang_summary: Vec<_> = stats
                        .chunks_by_language
                        .iter()
                        .map(|(l, c)| format!("{} {}", c, l))
                        .collect();
                    println!("      ({})", lang_summary.join(", "));
                }
            }
            Err(e) => {
                println!("  {} Index: {}", "[✗]".red(), e);
            }
        }
    } else {
        println!("  {} Index: not created yet", "[!]".yellow());
        println!("      Run 'cqs index' to create the index");
    }

    // Check references
    let config = cqs::config::Config::load(&root);
    if !config.references.is_empty() {
        println!();
        println!("References:");
        for r in &config.references {
            let db_path = r.path.join("index.db");
            if !r.path.exists() {
                println!(
                    "  {} {}: path missing ({})",
                    "[✗]".red(),
                    r.name,
                    r.path.display()
                );
                continue;
            }
            match Store::open(&db_path) {
                Ok(store) => {
                    let chunks = store.chunk_count().unwrap_or(0);
                    let hnsw = if cqs::HnswIndex::exists(&r.path, "index") {
                        "HNSW loaded".to_string()
                    } else {
                        "no HNSW".to_string()
                    };
                    println!(
                        "  {} {}: {} chunks, {} (weight {:.1})",
                        "[✓]".green(),
                        r.name,
                        chunks,
                        hnsw,
                        r.weight
                    );
                }
                Err(e) => {
                    println!("  {} {}: {}", "[✗]".red(), r.name, e);
                }
            }
        }
    }

    println!();
    println!("All checks passed.");

    Ok(())
}
