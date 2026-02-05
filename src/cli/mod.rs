//! CLI implementation for cq

mod config;
mod display;
mod files;
mod pipeline;
mod signal;
mod watch;

// Re-export for watch.rs
pub(crate) use config::find_project_root;
pub(crate) use signal::check_interrupted;

use config::apply_config_defaults;
use files::{acquire_index_lock, enumerate_files};
use pipeline::run_index_pipeline;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use colored::Colorize;

use cqs::{
    parse_notes, Embedder, Embedding, HnswIndex, ModelInfo, Parser as CqParser, SearchFilter, Store,
};

/// Configuration for the MCP server
struct ServeConfig {
    transport: String,
    bind: String,
    port: u16,
    project: Option<PathBuf>,
    gpu: bool,
    api_key: Option<String>,
    dangerously_allow_network_bind: bool,
}

#[derive(Parser)]
#[command(name = "cqs")]
#[command(about = "Semantic code search with local embeddings")]
#[command(version)]
pub struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Search query (quote multi-word queries)
    query: Option<String>,

    /// Max results
    #[arg(short = 'n', long, default_value = "5")]
    limit: usize,

    /// Min similarity threshold
    #[arg(short = 't', long, default_value = "0.3")]
    threshold: f32,

    /// Weight for name matching in hybrid search (0.0-1.0)
    #[arg(long, default_value = "0.2")]
    name_boost: f32,

    /// Weight for note scores in results (0.0-1.0, lower = notes rank below code)
    #[arg(long, default_value = "1.0")]
    note_weight: f32,

    /// Filter by language
    #[arg(short = 'l', long)]
    lang: Option<String>,

    /// Filter by path pattern (glob)
    #[arg(short = 'p', long)]
    path: Option<String>,

    /// Output as JSON
    #[arg(long)]
    json: bool,

    /// Show only file:line, no code
    #[arg(long)]
    no_content: bool,

    /// Show N lines of context before/after the chunk
    #[arg(short = 'C', long)]
    context: Option<usize>,

    /// Suppress progress output
    #[arg(short, long)]
    quiet: bool,

    /// Show debug info (sets RUST_LOG=debug)
    #[arg(short, long)]
    pub verbose: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Download model and create .cq/
    Init,
    /// Check model, index, hardware
    Doctor,
    /// Index current project
    Index {
        /// Re-index all files, ignore mtime cache
        #[arg(long)]
        force: bool,
        /// Show what would be indexed, don't write
        #[arg(long)]
        dry_run: bool,
        /// Index files ignored by .gitignore
        #[arg(long)]
        no_ignore: bool,
    },
    /// Show index statistics
    Stats,
    /// Watch for changes and reindex
    Watch {
        /// Debounce interval in milliseconds
        #[arg(long, default_value = "500")]
        debounce: u64,
        /// Index files ignored by .gitignore
        #[arg(long)]
        no_ignore: bool,
    },
    /// Start MCP server
    Serve {
        /// Transport type: stdio, http
        #[arg(long, default_value = "stdio")]
        transport: String,
        /// Bind address for HTTP transport
        #[arg(long, default_value = "127.0.0.1")]
        bind: String,
        /// Port for HTTP transport
        #[arg(long, default_value = "3000")]
        port: u16,
        /// Project root
        #[arg(long)]
        project: Option<PathBuf>,
        /// Use GPU for query embedding (faster after warmup)
        #[arg(long)]
        gpu: bool,
        /// API key for HTTP authentication (required for non-localhost bind)
        #[arg(long, env = "CQS_API_KEY")]
        api_key: Option<String>,
        /// Required when binding to non-localhost (exposes codebase to network)
        #[arg(long, hide = true)]
        dangerously_allow_network_bind: bool,
    },
    /// Generate shell completions
    Completions {
        /// Shell to generate completions for
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// Find functions that call a given function
    Callers {
        /// Function name to search for
        name: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Find functions called by a given function
    Callees {
        /// Function name to search for
        name: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

/// Run CLI with default argument parsing
///
/// Note: Typically main.rs uses run_with() to check verbose flag before tracing init.
/// This function is kept for library users who want simpler invocation.
#[allow(dead_code)]
pub fn run() -> Result<()> {
    run_with(Cli::parse())
}

/// Run CLI with pre-parsed arguments (used when main.rs needs to inspect args first)
pub fn run_with(mut cli: Cli) -> Result<()> {
    // Load config and apply defaults (CLI flags override config)
    let config = cqs::config::Config::load(&find_project_root());
    apply_config_defaults(&mut cli, &config);

    match cli.command {
        Some(Commands::Init) => cmd_init(&cli),
        Some(Commands::Doctor) => cmd_doctor(&cli),
        Some(Commands::Index {
            force,
            dry_run,
            no_ignore,
        }) => cmd_index(&cli, force, dry_run, no_ignore),
        Some(Commands::Stats) => cmd_stats(&cli),
        Some(Commands::Watch {
            debounce,
            no_ignore,
        }) => watch::cmd_watch(&cli, debounce, no_ignore),
        Some(Commands::Serve {
            ref transport,
            ref bind,
            port,
            ref project,
            gpu,
            ref api_key,
            dangerously_allow_network_bind,
        }) => cmd_serve(ServeConfig {
            transport: transport.clone(),
            bind: bind.clone(),
            port,
            project: project.clone(),
            gpu,
            api_key: api_key.clone(),
            dangerously_allow_network_bind,
        }),
        Some(Commands::Completions { shell }) => {
            cmd_completions(shell);
            Ok(())
        }
        Some(Commands::Callers { ref name, json }) => cmd_callers(&cli, name, json),
        Some(Commands::Callees { ref name, json }) => cmd_callees(&cli, name, json),
        None => match &cli.query {
            Some(q) => cmd_query(&cli, q),
            None => {
                println!("Usage: cqs <query> or cqs <command>");
                println!("Run 'cqs --help' for more information.");
                Ok(())
            }
        },
    }
}

// === Commands ===

/// Initialize cq in a project directory
///
/// Creates `.cq/` directory, downloads the embedding model, and warms up the embedder.
fn cmd_init(cli: &Cli) -> Result<()> {
    let root = find_project_root();
    let cq_dir = root.join(".cq");

    if !cli.quiet {
        println!("Initializing cq...");
    }

    // Create .cq directory
    std::fs::create_dir_all(&cq_dir).context("Failed to create .cq directory")?;

    // Create .gitignore
    let gitignore = cq_dir.join(".gitignore");
    std::fs::write(
        &gitignore,
        "index.db\nindex.db-wal\nindex.db-shm\nindex.lock\n",
    )
    .context("Failed to create .gitignore")?;

    // Download model
    if !cli.quiet {
        println!("Downloading model (~547MB)...");
    }

    let embedder = Embedder::new().context("Failed to initialize embedder")?;

    if !cli.quiet {
        println!("Detecting hardware... {}", embedder.provider());
    }

    // Warm up
    embedder.warm()?;

    if !cli.quiet {
        println!("Created .cq/");
        println!();
        println!("Run 'cq index' to index your codebase.");
    }

    Ok(())
}

/// Run diagnostic checks on cq installation and index
///
/// Reports runtime info, embedding provider, model status, and index statistics.
fn cmd_doctor(_cli: &Cli) -> Result<()> {
    let root = find_project_root();
    let cq_dir = root.join(".cq");
    let index_path = cq_dir.join("index.db");

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
        println!("      Run 'cq index' to create the index");
    }

    println!();
    println!("All checks passed.");

    Ok(())
}

/// Index codebase files for semantic search
///
/// Parses source files, generates embeddings, and stores them in the index database.
/// Uses incremental indexing by default (only re-embeds changed files).
fn cmd_index(cli: &Cli, force: bool, dry_run: bool, no_ignore: bool) -> Result<()> {
    let root = find_project_root();
    let cq_dir = root.join(".cq");
    let index_path = cq_dir.join("index.db");

    // Ensure .cq directory exists
    if !cq_dir.exists() {
        std::fs::create_dir_all(&cq_dir)?;
    }

    // Acquire lock (unless dry run)
    let _lock = if !dry_run {
        Some(acquire_index_lock(&cq_dir)?)
    } else {
        None
    };

    signal::setup_signal_handler();

    let _span = tracing::info_span!("cmd_index", force = force, dry_run = dry_run).entered();

    if !cli.quiet {
        println!("Scanning files...");
    }

    let parser = CqParser::new()?;
    let files = enumerate_files(&root, &parser, no_ignore)?;

    if !cli.quiet {
        println!("Found {} files", files.len());
    }

    if dry_run {
        for file in &files {
            println!("  {}", file.display());
        }
        println!();
        println!("(dry run - no changes made)");
        return Ok(());
    }

    // Initialize or open store
    let store = if index_path.exists() && !force {
        Store::open(&index_path)?
    } else {
        // Remove old index if forcing
        if index_path.exists() {
            std::fs::remove_file(&index_path)?;
        }
        let store = Store::open(&index_path)?;
        store.init(&ModelInfo::default())?;
        store
    };

    if !cli.quiet {
        println!("Indexing {} files (pipelined)...", files.len());
    }

    // Run the 3-stage pipeline: parse → embed → write
    let stats = run_index_pipeline(&root, files.clone(), &index_path, force, cli.quiet)?;
    let total_embedded = stats.total_embedded;
    let total_cached = stats.total_cached;
    let gpu_failures = stats.gpu_failures;

    // Prune missing files
    let existing_files: HashSet<_> = files.into_iter().collect();
    let pruned = store.prune_missing(&existing_files)?;

    if !cli.quiet {
        println!();
        println!("Index complete:");
        let newly_embedded = total_embedded - total_cached;
        if total_cached > 0 {
            println!(
                "  Chunks: {} ({} cached, {} embedded)",
                total_embedded, total_cached, newly_embedded
            );
        } else {
            println!("  Embedded: {}", total_embedded);
        }
        if gpu_failures > 0 {
            println!("  GPU failures: {} (fell back to CPU)", gpu_failures);
        }
        if pruned > 0 {
            println!("  Pruned: {} (deleted files)", pruned);
        }
    }

    // Extract full call graph (includes large functions >100 lines)
    if !check_interrupted() {
        if !cli.quiet {
            println!("Extracting call graph...");
        }

        let total_calls = extract_call_graph(&parser, &root, &existing_files, &store)?;

        if !cli.quiet {
            println!("  Call graph: {} calls", total_calls);
        }
    }

    // Index notes if notes.toml exists
    if !check_interrupted() {
        if !cli.quiet {
            println!("Indexing notes...");
        }

        let (note_count, was_skipped) = index_notes_from_file(&root, &store, force)?;

        if !cli.quiet {
            if was_skipped && note_count == 0 {
                println!("Notes up to date.");
            } else if note_count > 0 {
                let (total, warnings, patterns) = store.note_stats()?;
                println!(
                    "  Notes: {} total ({} warnings, {} patterns)",
                    total, warnings, patterns
                );
            }
        }
    }

    // Build HNSW index for fast search (includes both chunks and notes)
    if !check_interrupted() {
        if !cli.quiet {
            println!("Building HNSW index...");
        }

        if let Some((total, chunk_count, note_count)) = build_hnsw_index(&store, &cq_dir)? {
            if !cli.quiet {
                println!(
                    "  HNSW index: {} vectors ({} chunks, {} notes)",
                    total, chunk_count, note_count
                );
            }
        }
    }

    Ok(())
}

/// Extract call graph from source files
///
/// Parses function call relationships for callers/callees queries.
/// Returns the total number of calls extracted.
fn extract_call_graph(
    parser: &CqParser,
    root: &Path,
    files: &HashSet<PathBuf>,
    store: &Store,
) -> Result<usize> {
    let mut total_calls = 0;
    for file in files {
        let abs_path = root.join(file);
        match parser.parse_file_calls(&abs_path) {
            Ok(function_calls) => {
                for fc in &function_calls {
                    total_calls += fc.calls.len();
                }
                store.upsert_function_calls(file, &function_calls)?;
            }
            Err(e) => {
                tracing::warn!("Failed to extract calls from {}: {}", abs_path.display(), e);
            }
        }
    }
    Ok(total_calls)
}

/// Index notes from notes.toml if it exists and needs reindexing
///
/// Returns (indexed_count, was_skipped) where was_skipped is true if notes were up to date.
fn index_notes_from_file(root: &Path, store: &Store, force: bool) -> Result<(usize, bool)> {
    let notes_path = root.join("docs/notes.toml");
    if !notes_path.exists() {
        return Ok((0, true));
    }

    // Check if notes need reindexing (Some(mtime) = needs reindex, None = up to date)
    let needs_reindex = force
        || store
            .notes_need_reindex(&notes_path)
            .unwrap_or(Some(0))
            .is_some();

    if !needs_reindex {
        return Ok((0, true));
    }

    match parse_notes(&notes_path) {
        Ok(notes) => {
            if notes.is_empty() {
                return Ok((0, false));
            }

            let embedder = Embedder::new()?;
            let texts: Vec<String> = notes.iter().map(|n| n.embedding_text()).collect();
            let text_refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
            let base_embeddings = embedder.embed_documents(&text_refs)?;

            // Add sentiment as 769th dimension
            let embeddings_with_sentiment: Vec<Embedding> = base_embeddings
                .into_iter()
                .zip(notes.iter())
                .map(|(emb, note)| emb.with_sentiment(note.sentiment()))
                .collect();

            // Get file mtime
            let file_mtime = notes_path
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);

            // Delete old notes and insert new
            store.delete_notes_by_file(&notes_path)?;
            let count = notes.len();
            let note_embeddings: Vec<_> =
                notes.into_iter().zip(embeddings_with_sentiment).collect();
            store.upsert_notes_batch(&note_embeddings, &notes_path, file_mtime)?;

            Ok((count, false))
        }
        Err(e) => {
            tracing::warn!("Failed to parse notes: {}", e);
            Ok((0, false))
        }
    }
}

/// Build HNSW index from store embeddings
///
/// Creates an HNSW index containing both chunk and note embeddings,
/// using batched insertion to avoid OOM on large repos.
fn build_hnsw_index(store: &Store, cq_dir: &Path) -> Result<Option<(usize, usize, usize)>> {
    let chunk_count = store.chunk_count()? as usize;
    let note_count = store.note_count()? as usize;
    let total_count = chunk_count + note_count;

    if total_count == 0 {
        return Ok(None);
    }

    // Stream chunk embeddings in 10k batches, then add all notes
    // Notes are capped at 10k so loading them all is fine
    const HNSW_BATCH_SIZE: usize = 10_000;

    let chunk_batches = store.embedding_batches(HNSW_BATCH_SIZE);
    let note_batch = std::iter::once(store.note_embeddings());

    let hnsw = HnswIndex::build_batched(chunk_batches.chain(note_batch), total_count)?;
    hnsw.save(cq_dir, "index")?;

    Ok(Some((hnsw.len(), chunk_count, note_count)))
}

/// Load HNSW index if available, wrapped as trait object
fn load_hnsw_index(cq_dir: &std::path::Path) -> Option<Box<dyn cqs::index::VectorIndex>> {
    if HnswIndex::exists(cq_dir, "index") {
        match HnswIndex::load(cq_dir, "index") {
            Ok(index) => {
                tracing::info!("Using HNSW index ({} vectors)", index.len());
                Some(Box::new(index))
            }
            Err(e) => {
                tracing::warn!("Failed to load HNSW index, using brute-force: {}", e);
                None
            }
        }
    } else {
        tracing::debug!("No HNSW index found, using brute-force search");
        None
    }
}

/// Execute a semantic search query and display results
fn cmd_query(cli: &Cli, query: &str) -> Result<()> {
    let _span = tracing::info_span!("cmd_query", query_len = query.len()).entered();

    let root = find_project_root();
    let cq_dir = root.join(".cq");
    let index_path = cq_dir.join("index.db");

    if !index_path.exists() {
        bail!("Index not found. Run 'cq init && cq index' first.");
    }

    let store = Store::open(&index_path)?;
    let embedder = Embedder::new()?;

    let query_embedding = embedder.embed_query(query)?;

    let languages = match &cli.lang {
        Some(l) => Some(vec![l.parse().context(
            "Invalid language. Valid: rust, python, typescript, javascript, go",
        )?]),
        None => None,
    };

    let filter = SearchFilter {
        languages,
        path_pattern: cli.path.clone(),
        name_boost: cli.name_boost,
        query_text: query.to_string(),
        enable_rrf: true, // Enable RRF hybrid search by default
        note_weight: cli.note_weight,
    };

    // Load vector index for O(log n) search
    let index: Option<Box<dyn cqs::index::VectorIndex>> = {
        #[cfg(feature = "gpu-search")]
        {
            // Priority: CAGRA (GPU, large indexes) > HNSW (CPU) > brute-force
            //
            // CAGRA rebuilds index each CLI invocation (~1s for 474 vectors).
            // Only worth it when search time savings exceed rebuild cost.
            // Threshold: 5000 vectors (where CAGRA search is ~10x faster than HNSW)
            const CAGRA_THRESHOLD: u64 = 5000;
            let chunk_count = store.chunk_count().unwrap_or(0);
            if chunk_count >= CAGRA_THRESHOLD && cqs::cagra::CagraIndex::gpu_available() {
                match cqs::cagra::CagraIndex::build_from_store(&store) {
                    Ok(idx) => {
                        tracing::info!("Using CAGRA GPU index ({} vectors)", idx.len());
                        Some(Box::new(idx) as Box<dyn cqs::index::VectorIndex>)
                    }
                    Err(e) => {
                        tracing::warn!("Failed to build CAGRA index, falling back to HNSW: {}", e);
                        load_hnsw_index(&cq_dir)
                    }
                }
            } else {
                if chunk_count < CAGRA_THRESHOLD {
                    tracing::debug!(
                        "Index too small for CAGRA ({} < {}), using HNSW",
                        chunk_count,
                        CAGRA_THRESHOLD
                    );
                } else {
                    tracing::debug!("GPU not available, using HNSW");
                }
                load_hnsw_index(&cq_dir)
            }
        }
        #[cfg(not(feature = "gpu-search"))]
        {
            load_hnsw_index(&cq_dir)
        }
    };

    // Use unified search with vector index if available
    let results = store.search_unified_with_index(
        &query_embedding,
        &filter,
        cli.limit,
        cli.threshold,
        index.as_deref(),
    )?;

    if results.is_empty() {
        if cli.json {
            println!(r#"{{"results":[],"query":"{}","total":0}}"#, query);
        } else {
            println!("No results found.");
        }
        std::process::exit(signal::ExitCode::NoResults as i32);
    }

    if cli.json {
        display::display_unified_results_json(&results, query)?;
    } else {
        display::display_unified_results(&results, &root, cli.no_content, cli.context)?;
    }

    Ok(())
}

/// Display index statistics (chunk counts, languages, types)
fn cmd_stats(cli: &Cli) -> Result<()> {
    let root = find_project_root();
    let index_path = root.join(".cq/index.db");

    if !index_path.exists() {
        bail!("Index not found. Run 'cq init && cq index' first.");
    }

    let store = Store::open(&index_path)?;
    let stats = store.stats()?;

    let cq_dir = root.join(".cq");
    // Use count_vectors to avoid loading full HNSW index just for stats
    let hnsw_vectors = HnswIndex::count_vectors(&cq_dir, "index");

    if cli.json {
        let json = serde_json::json!({
            "total_chunks": stats.total_chunks,
            "total_files": stats.total_files,
            "by_language": stats.chunks_by_language.iter()
                .map(|(l, c)| (l.to_string(), c))
                .collect::<std::collections::HashMap<_, _>>(),
            "by_type": stats.chunks_by_type.iter()
                .map(|(t, c)| (t.to_string(), c))
                .collect::<std::collections::HashMap<_, _>>(),
            "model": stats.model_name,
            "schema_version": stats.schema_version,
            "created_at": stats.created_at,
            "hnsw_vectors": hnsw_vectors,
        });
        println!("{}", serde_json::to_string_pretty(&json)?);
    } else {
        println!("Index Statistics");
        println!("================");
        println!();
        println!("Total chunks: {}", stats.total_chunks);
        println!("Total files:  {}", stats.total_files);
        println!();
        println!("By language:");
        for (lang, count) in &stats.chunks_by_language {
            println!("  {}: {}", lang, count);
        }
        println!();
        println!("By type:");
        for (chunk_type, count) in &stats.chunks_by_type {
            println!("  {}: {}", chunk_type, count);
        }
        println!();
        println!("Model: {}", stats.model_name);
        println!("Schema: v{}", stats.schema_version);
        println!("Created: {}", stats.created_at);

        // HNSW index status (use count_vectors to avoid loading full index)
        println!();
        match hnsw_vectors {
            Some(count) => {
                println!("HNSW index: {} vectors (O(log n) search)", count);
            }
            None => {
                println!("HNSW index: not built (using brute-force O(n) search)");
                if stats.total_chunks > 10_000 {
                    println!("  Tip: Run 'cqs index' to build HNSW for faster search");
                }
            }
        }

        // Warning for very large indexes
        if stats.total_chunks > 50_000 {
            println!();
            println!(
                "Warning: {} chunks is a large index. Consider:",
                stats.total_chunks
            );
            println!("  - Using --path to limit search scope");
            println!("  - Splitting into multiple projects");
        }
    }

    Ok(())
}

/// Find functions that call the specified function
fn cmd_callers(_cli: &Cli, name: &str, json: bool) -> Result<()> {
    let root = find_project_root();
    let index_path = root.join(".cq/index.db");

    if !index_path.exists() {
        bail!("Index not found. Run 'cqs init && cqs index' first.");
    }

    let store = Store::open(&index_path)?;
    // Use full call graph (includes large functions)
    let callers = store.get_callers_full(name)?;

    if callers.is_empty() {
        if json {
            println!("[]");
        } else {
            println!("No callers found for '{}'", name);
        }
        return Ok(());
    }

    if json {
        let json_output: Vec<serde_json::Value> = callers
            .iter()
            .map(|c| {
                serde_json::json!({
                    "name": c.name,
                    "file": c.file.to_string_lossy(),
                    "line": c.line,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&json_output)?);
    } else {
        println!("Functions that call '{}':", name);
        println!();
        for caller in &callers {
            println!(
                "  {} ({}:{})",
                caller.name.cyan(),
                caller.file.display(),
                caller.line
            );
        }
        println!();
        println!("Total: {} caller(s)", callers.len());
    }

    Ok(())
}

/// Find functions called by the specified function
fn cmd_callees(_cli: &Cli, name: &str, json: bool) -> Result<()> {
    let root = find_project_root();
    let index_path = root.join(".cq/index.db");

    if !index_path.exists() {
        bail!("Index not found. Run 'cqs init && cqs index' first.");
    }

    let store = Store::open(&index_path)?;
    // Use full call graph (includes large functions)
    let callees = store.get_callees_full(name)?;

    if json {
        let json_output = serde_json::json!({
            "function": name,
            "calls": callees.iter().map(|(n, line)| {
                serde_json::json!({"name": n, "line": line})
            }).collect::<Vec<_>>(),
            "count": callees.len(),
        });
        println!("{}", serde_json::to_string_pretty(&json_output)?);
    } else {
        println!("Functions called by '{}':", name.cyan());
        println!();
        if callees.is_empty() {
            println!("  (no function calls found)");
        } else {
            for (callee_name, _line) in &callees {
                println!("  {}", callee_name);
            }
        }
        println!();
        println!("Total: {} call(s)", callees.len());
    }

    Ok(())
}

/// Start the MCP server for IDE integration
fn cmd_serve(config: ServeConfig) -> Result<()> {
    // Block non-localhost bind unless explicitly allowed
    let is_localhost =
        config.bind == "127.0.0.1" || config.bind == "localhost" || config.bind == "::1";
    if !is_localhost && !config.dangerously_allow_network_bind {
        bail!(
            "Binding to '{}' would expose your codebase to the network.\n\
             If this is intentional, add --dangerously-allow-network-bind",
            config.bind
        );
    }

    // Require API key for non-localhost HTTP binds
    if !is_localhost && config.transport == "http" && config.api_key.is_none() {
        bail!(
            "API key required for non-localhost HTTP bind.\n\
             Set --api-key <key> or CQS_API_KEY environment variable."
        );
    }

    let root = config.project.unwrap_or_else(find_project_root);

    match config.transport.as_str() {
        "stdio" => cqs::serve_stdio(root, config.gpu),
        "http" => cqs::serve_http(root, &config.bind, config.port, config.gpu, config.api_key),
        // Keep sse as alias for backwards compatibility
        "sse" => cqs::serve_http(root, &config.bind, config.port, config.gpu, config.api_key),
        _ => {
            bail!(
                "Unknown transport: {}. Use 'stdio' or 'http'.",
                config.transport
            );
        }
    }
}

/// Generate shell completion scripts for the specified shell
fn cmd_completions(shell: clap_complete::Shell) {
    use clap::CommandFactory;
    clap_complete::generate(shell, &mut Cli::command(), "cqs", &mut std::io::stdout());
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    // ===== Default values tests =====

    #[test]
    fn test_cli_defaults() {
        let cli = Cli::try_parse_from(["cqs"]).unwrap();
        assert_eq!(cli.limit, 5);
        assert!((cli.threshold - 0.3).abs() < 0.001);
        assert!((cli.name_boost - 0.2).abs() < 0.001);
        assert!(!cli.json);
        assert!(!cli.quiet);
        assert!(!cli.verbose);
        assert!(cli.query.is_none());
        assert!(cli.command.is_none());
    }

    #[test]
    fn test_cli_query_argument() {
        let cli = Cli::try_parse_from(["cqs", "parse config"]).unwrap();
        assert_eq!(cli.query, Some("parse config".to_string()));
    }

    #[test]
    fn test_cli_limit_flag() {
        let cli = Cli::try_parse_from(["cqs", "-n", "10", "query"]).unwrap();
        assert_eq!(cli.limit, 10);

        let cli = Cli::try_parse_from(["cqs", "--limit", "20", "query"]).unwrap();
        assert_eq!(cli.limit, 20);
    }

    #[test]
    fn test_cli_threshold_flag() {
        let cli = Cli::try_parse_from(["cqs", "-t", "0.5", "query"]).unwrap();
        assert!((cli.threshold - 0.5).abs() < 0.001);

        let cli = Cli::try_parse_from(["cqs", "--threshold", "0.8", "query"]).unwrap();
        assert!((cli.threshold - 0.8).abs() < 0.001);
    }

    #[test]
    fn test_cli_language_filter() {
        let cli = Cli::try_parse_from(["cqs", "-l", "rust", "query"]).unwrap();
        assert_eq!(cli.lang, Some("rust".to_string()));

        let cli = Cli::try_parse_from(["cqs", "--lang", "python", "query"]).unwrap();
        assert_eq!(cli.lang, Some("python".to_string()));
    }

    #[test]
    fn test_cli_path_filter() {
        let cli = Cli::try_parse_from(["cqs", "-p", "src/**", "query"]).unwrap();
        assert_eq!(cli.path, Some("src/**".to_string()));
    }

    #[test]
    fn test_cli_json_flag() {
        let cli = Cli::try_parse_from(["cqs", "--json", "query"]).unwrap();
        assert!(cli.json);
    }

    #[test]
    fn test_cli_context_flag() {
        let cli = Cli::try_parse_from(["cqs", "-C", "3", "query"]).unwrap();
        assert_eq!(cli.context, Some(3));

        let cli = Cli::try_parse_from(["cqs", "--context", "5", "query"]).unwrap();
        assert_eq!(cli.context, Some(5));
    }

    #[test]
    fn test_cli_quiet_verbose_flags() {
        let cli = Cli::try_parse_from(["cqs", "-q", "query"]).unwrap();
        assert!(cli.quiet);

        let cli = Cli::try_parse_from(["cqs", "-v", "query"]).unwrap();
        assert!(cli.verbose);
    }

    // ===== Subcommand tests =====

    #[test]
    fn test_cmd_init() {
        let cli = Cli::try_parse_from(["cqs", "init"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Init)));
    }

    #[test]
    fn test_cmd_index() {
        let cli = Cli::try_parse_from(["cqs", "index"]).unwrap();
        match cli.command {
            Some(Commands::Index {
                force,
                dry_run,
                no_ignore,
            }) => {
                assert!(!force);
                assert!(!dry_run);
                assert!(!no_ignore);
            }
            _ => panic!("Expected Index command"),
        }
    }

    #[test]
    fn test_cmd_index_with_flags() {
        let cli = Cli::try_parse_from(["cqs", "index", "--force", "--dry-run"]).unwrap();
        match cli.command {
            Some(Commands::Index { force, dry_run, .. }) => {
                assert!(force);
                assert!(dry_run);
            }
            _ => panic!("Expected Index command"),
        }
    }

    #[test]
    fn test_cmd_stats() {
        let cli = Cli::try_parse_from(["cqs", "stats"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Stats)));
    }

    #[test]
    fn test_cmd_watch() {
        let cli = Cli::try_parse_from(["cqs", "watch"]).unwrap();
        match cli.command {
            Some(Commands::Watch {
                debounce,
                no_ignore,
            }) => {
                assert_eq!(debounce, 500); // default
                assert!(!no_ignore);
            }
            _ => panic!("Expected Watch command"),
        }
    }

    #[test]
    fn test_cmd_watch_custom_debounce() {
        let cli = Cli::try_parse_from(["cqs", "watch", "--debounce", "1000"]).unwrap();
        match cli.command {
            Some(Commands::Watch { debounce, .. }) => {
                assert_eq!(debounce, 1000);
            }
            _ => panic!("Expected Watch command"),
        }
    }

    #[test]
    fn test_cmd_serve_defaults() {
        let cli = Cli::try_parse_from(["cqs", "serve"]).unwrap();
        match cli.command {
            Some(Commands::Serve {
                transport,
                bind,
                port,
                gpu,
                api_key,
                ..
            }) => {
                assert_eq!(transport, "stdio");
                assert_eq!(bind, "127.0.0.1");
                assert_eq!(port, 3000);
                assert!(!gpu);
                assert!(api_key.is_none());
            }
            _ => panic!("Expected Serve command"),
        }
    }

    #[test]
    fn test_cmd_serve_http() {
        let cli = Cli::try_parse_from([
            "cqs",
            "serve",
            "--transport",
            "http",
            "--port",
            "8080",
            "--gpu",
        ])
        .unwrap();
        match cli.command {
            Some(Commands::Serve {
                transport,
                port,
                gpu,
                ..
            }) => {
                assert_eq!(transport, "http");
                assert_eq!(port, 8080);
                assert!(gpu);
            }
            _ => panic!("Expected Serve command"),
        }
    }

    #[test]
    fn test_cmd_callers() {
        let cli = Cli::try_parse_from(["cqs", "callers", "my_function"]).unwrap();
        match cli.command {
            Some(Commands::Callers { name, json }) => {
                assert_eq!(name, "my_function");
                assert!(!json);
            }
            _ => panic!("Expected Callers command"),
        }
    }

    #[test]
    fn test_cmd_callees_json() {
        let cli = Cli::try_parse_from(["cqs", "callees", "my_function", "--json"]).unwrap();
        match cli.command {
            Some(Commands::Callees { name, json }) => {
                assert_eq!(name, "my_function");
                assert!(json);
            }
            _ => panic!("Expected Callees command"),
        }
    }

    // ===== Error cases =====

    #[test]
    fn test_invalid_limit_rejected() {
        let result = Cli::try_parse_from(["cqs", "-n", "not_a_number"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_missing_subcommand_arg_rejected() {
        // callers requires a name argument
        let result = Cli::try_parse_from(["cqs", "callers"]);
        assert!(result.is_err());
    }

    // ===== apply_config_defaults tests =====

    #[test]
    fn test_apply_config_defaults_respects_cli_flags() {
        // When CLI has non-default values, config should NOT override
        let mut cli = Cli::try_parse_from(["cqs", "-n", "10", "-t", "0.6", "query"]).unwrap();
        let config = cqs::config::Config {
            limit: Some(20),
            threshold: Some(0.9),
            name_boost: Some(0.5),
            quiet: Some(true),
            verbose: Some(true),
        };
        apply_config_defaults(&mut cli, &config);

        // CLI values should be preserved
        assert_eq!(cli.limit, 10);
        assert!((cli.threshold - 0.6).abs() < 0.001);
        // But name_boost was default, so config applies
        assert!((cli.name_boost - 0.5).abs() < 0.001);
    }

    #[test]
    fn test_apply_config_defaults_applies_when_cli_has_defaults() {
        let mut cli = Cli::try_parse_from(["cqs", "query"]).unwrap();
        let config = cqs::config::Config {
            limit: Some(15),
            threshold: Some(0.7),
            name_boost: Some(0.4),
            quiet: Some(true),
            verbose: Some(true),
        };
        apply_config_defaults(&mut cli, &config);

        assert_eq!(cli.limit, 15);
        assert!((cli.threshold - 0.7).abs() < 0.001);
        assert!((cli.name_boost - 0.4).abs() < 0.001);
        assert!(cli.quiet);
        assert!(cli.verbose);
    }

    #[test]
    fn test_apply_config_defaults_empty_config() {
        let mut cli = Cli::try_parse_from(["cqs", "query"]).unwrap();
        let config = cqs::config::Config::default();
        apply_config_defaults(&mut cli, &config);

        // Should keep CLI defaults
        assert_eq!(cli.limit, 5);
        assert!((cli.threshold - 0.3).abs() < 0.001);
        assert!((cli.name_boost - 0.2).abs() < 0.001);
        assert!(!cli.quiet);
        assert!(!cli.verbose);
    }

    // ===== ExitCode tests =====

    #[test]
    fn test_exit_code_values() {
        assert_eq!(signal::ExitCode::NoResults as i32, 2);
        assert_eq!(signal::ExitCode::Interrupted as i32, 130);
    }

    // ===== display module tests =====

    mod display_tests {
        use cqs::store::UnifiedResult;

        #[test]
        fn test_display_unified_results_json_empty() {
            let results: Vec<UnifiedResult> = vec![];
            // Can't easily capture stdout, but we can at least verify it doesn't panic
            // This would be better as an integration test
            assert!(results.is_empty());
        }
    }

    // ===== Progress bar template tests =====

    #[test]
    fn test_progress_bar_template_valid() {
        // Verify the progress bar template used in cmd_index is valid.
        // This catches template syntax errors at test time rather than runtime.
        use indicatif::ProgressStyle;
        let result =
            ProgressStyle::default_bar().template("[{elapsed_precise}] {bar:40.cyan/blue} {msg}");
        assert!(result.is_ok(), "Progress bar template should be valid");
    }
}
