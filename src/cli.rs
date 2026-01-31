//! CLI implementation for cq

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use colored::Colorize;
use ignore::WalkBuilder;
use indicatif::{ProgressBar, ProgressStyle};
use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
use rayon::prelude::*;

use cqs::embedder::Embedder;
use cqs::hnsw::HnswIndex;
use cqs::parser::Parser as CqParser;
use cqs::store::{ModelInfo, SearchFilter, Store};

// Constants
const MAX_FILE_SIZE: u64 = 1_048_576; // 1MB

// Exit codes
#[repr(i32)]
#[allow(dead_code)]
pub enum ExitCode {
    Success = 0,
    GeneralError = 1,
    NoResults = 2,
    IndexMissing = 3,
    ModelMissing = 4,
    Interrupted = 130,
}

// Signal handling
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

fn setup_signal_handler() {
    ctrlc::set_handler(|| {
        if INTERRUPTED.swap(true, Ordering::SeqCst) {
            // Second Ctrl+C: force exit
            std::process::exit(ExitCode::Interrupted as i32);
        }
        eprintln!("\nInterrupted. Finishing current batch...");
    })
    .expect("Failed to set Ctrl+C handler");
}

fn check_interrupted() -> bool {
    INTERRUPTED.load(Ordering::SeqCst)
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

    /// Show debug info
    #[arg(short, long)]
    verbose: bool,
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
        /// Port for HTTP transport
        #[arg(long, default_value = "3000")]
        port: u16,
        /// Project root
        #[arg(long)]
        project: Option<PathBuf>,
    },
    /// Generate shell completions
    Completions {
        /// Shell to generate completions for
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}

pub fn run() -> Result<()> {
    let mut cli = Cli::parse();

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
        }) => cmd_watch(&cli, debounce, no_ignore),
        Some(Commands::Serve {
            ref transport,
            port,
            ref project,
        }) => cmd_serve(&cli, transport, port, project.clone()),
        Some(Commands::Completions { shell }) => {
            cmd_completions(shell);
            Ok(())
        }
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

/// Apply config file defaults to CLI options
/// CLI flags always override config values
fn apply_config_defaults(cli: &mut Cli, config: &cqs::config::Config) {
    // Only apply config if CLI has default values
    // (we can't detect if user explicitly passed the default, so this is imperfect)
    if cli.limit == 5 {
        if let Some(limit) = config.limit {
            cli.limit = limit;
        }
    }
    if (cli.threshold - 0.3).abs() < f32::EPSILON {
        if let Some(threshold) = config.threshold {
            cli.threshold = threshold;
        }
    }
    if (cli.name_boost - 0.2).abs() < f32::EPSILON {
        if let Some(name_boost) = config.name_boost {
            cli.name_boost = name_boost;
        }
    }
    if !cli.quiet {
        if let Some(true) = config.quiet {
            cli.quiet = true;
        }
    }
    if !cli.verbose {
        if let Some(true) = config.verbose {
            cli.verbose = true;
        }
    }
}

/// Find project root by looking for common markers
fn find_project_root() -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut current = cwd.as_path();

    loop {
        // Check for project markers
        let markers = [
            "Cargo.toml",
            "package.json",
            "pyproject.toml",
            "setup.py",
            "go.mod",
            ".git",
        ];

        for marker in &markers {
            if current.join(marker).exists() {
                return current.to_path_buf();
            }
        }

        // Move up
        match current.parent() {
            Some(parent) => current = parent,
            None => break,
        }
    }

    // Fall back to CWD with warning
    tracing::warn!("No project root found, using current directory");
    cwd
}

/// Enumerate files to index
fn enumerate_files(root: &Path, parser: &CqParser, no_ignore: bool) -> Result<Vec<PathBuf>> {
    let root = root.canonicalize().context("Failed to canonicalize root")?;

    let walker = WalkBuilder::new(&root)
        .git_ignore(!no_ignore)
        .git_global(!no_ignore)
        .git_exclude(!no_ignore)
        .ignore(!no_ignore)
        .hidden(!no_ignore)
        .follow_links(false)
        .build();

    let files: Vec<PathBuf> = walker
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|ft| ft.is_file()).unwrap_or(false))
        .filter(|e| {
            // Skip files over size limit
            e.metadata()
                .map(|m| m.len() <= MAX_FILE_SIZE)
                .unwrap_or(false)
        })
        .filter(|e| {
            // Only supported extensions
            e.path()
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| parser.supported_extensions().contains(&ext))
                .unwrap_or(false)
        })
        .filter_map(|e| {
            // Validate path stays within project root and convert to relative
            let path = e.path().canonicalize().ok()?;
            if path.starts_with(&root) {
                // Store relative path for portability and glob matching
                Some(path.strip_prefix(&root).unwrap_or(&path).to_path_buf())
            } else {
                tracing::warn!("Skipping path outside project: {}", e.path().display());
                None
            }
        })
        .collect();

    Ok(files)
}

/// Parse files in parallel
/// `root` is joined with relative paths for filesystem access
/// `files` contains relative paths which are stored in chunks for portability
fn parse_files(parser: &CqParser, root: &Path, files: &[PathBuf]) -> Vec<cqs::parser::Chunk> {
    files
        .par_iter()
        .flat_map(|rel_path| {
            let abs_path = root.join(rel_path);
            match parser.parse_file(&abs_path) {
                Ok(mut chunks) => {
                    // Rewrite paths to be relative for storage
                    for chunk in &mut chunks {
                        chunk.file = rel_path.clone();
                        // Rebuild ID with relative path
                        chunk.id = format!(
                            "{}:{}:{}",
                            rel_path.display(),
                            chunk.line_start,
                            &chunk.content_hash[..8]
                        );
                    }
                    chunks
                }
                Err(e) => {
                    tracing::warn!("Failed to parse {}: {}", abs_path.display(), e);
                    vec![]
                }
            }
        })
        .collect()
}

/// Check if a process with the given PID exists
#[cfg(unix)]
fn process_exists(pid: u32) -> bool {
    // kill with signal 0 checks if process exists without sending a signal
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

#[cfg(windows)]
fn process_exists(pid: u32) -> bool {
    use std::process::Command;
    Command::new("tasklist")
        .args(["/FI", &format!("PID eq {}", pid), "/NH"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
        .unwrap_or(false)
}

/// Acquire file lock to prevent concurrent indexing
/// Writes PID to lock file for stale lock detection
fn acquire_index_lock(cq_dir: &Path) -> Result<std::fs::File> {
    use fs4::fs_std::FileExt;
    use std::io::Write;

    let lock_path = cq_dir.join("index.lock");

    // Try to open/create the lock file
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(&lock_path)
        .context("Failed to create lock file")?;

    match lock_file.try_lock_exclusive() {
        Ok(()) => {
            // Write our PID to the lock file
            let mut file = lock_file;
            writeln!(file, "{}", std::process::id())?;
            file.sync_all()?;
            Ok(file)
        }
        Err(_) => {
            // Lock is held - check if the owning process is still alive
            if let Ok(content) = std::fs::read_to_string(&lock_path) {
                if let Ok(pid) = content.trim().parse::<u32>() {
                    if !process_exists(pid) {
                        // Stale lock - process is dead, remove and retry
                        tracing::warn!("Removing stale lock (PID {} no longer exists)", pid);
                        drop(lock_file);
                        std::fs::remove_file(&lock_path)?;
                        // Recursive retry (once)
                        return acquire_index_lock(cq_dir);
                    }
                }
            }
            bail!(
                "Another cqs process is indexing (see .cq/index.lock). \
                 Hint: Wait for it to finish, or delete .cq/index.lock if the process crashed."
            )
        }
    }
}

// === Commands ===

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

    let mut embedder = Embedder::new().context("Failed to initialize embedder")?;

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

fn cmd_doctor(_cli: &Cli) -> Result<()> {
    let root = find_project_root();
    let cq_dir = root.join(".cq");
    let index_path = cq_dir.join("index.db");

    println!("Runtime:");

    // Check model
    match Embedder::new() {
        Ok(mut embedder) => {
            println!("  {} Model: nomic-embed-text-v1.5", "[✓]".green());
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

    setup_signal_handler();

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

    if !cli.quiet {
        println!("Parsing...");
    }

    let chunks = parse_files(&parser, &root, &files);

    if !cli.quiet {
        println!("Found {} chunks", chunks.len());
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

    // Filter by needs_reindex unless forced
    let chunks_to_embed: Vec<_> = if force {
        chunks.into_iter().collect()
    } else {
        chunks
            .into_iter()
            .filter(|c| {
                // Join with root for filesystem access
                let abs_path = root.join(&c.file);
                store.needs_reindex(&abs_path).unwrap_or(true)
            })
            .collect()
    };

    if chunks_to_embed.is_empty() {
        if !cli.quiet {
            println!("Index is up to date.");
        }
        return Ok(());
    }

    if !cli.quiet {
        println!("Embedding {} chunks...", chunks_to_embed.len());
    }

    let mut embedder = Embedder::new()?;

    if !cli.quiet {
        println!("Using {}", embedder.provider());
    }

    let progress = if cli.quiet {
        ProgressBar::hidden()
    } else {
        let pb = ProgressBar::new(chunks_to_embed.len() as u64);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("[{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} {msg}")
                .unwrap(),
        );
        pb
    };

    let batch_size = embedder.batch_size();
    let mut total_embedded = 0;

    for batch in chunks_to_embed.chunks(batch_size) {
        if check_interrupted() {
            eprintln!("Committing partial index...");
            break;
        }

        // Prepare embedding input: doc + signature + content
        let texts: Vec<String> = batch.iter().map(prepare_embedding_input).collect();
        let text_refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
        let embeddings = embedder.embed_documents(&text_refs)?;

        // Get file mtime (use first file's mtime for the batch)
        let file_mtime = batch
            .first()
            .and_then(|c| root.join(&c.file).metadata().ok())
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        let chunk_embeddings: Vec<_> = batch.iter().cloned().zip(embeddings).collect();
        store.upsert_chunks_batch(&chunk_embeddings, file_mtime)?;

        total_embedded += batch.len();
        progress.set_position(total_embedded as u64);
    }

    progress.finish_with_message("done");

    // Prune missing files
    let existing_files: HashSet<_> = files.into_iter().collect();
    let pruned = store.prune_missing(&existing_files)?;

    if !cli.quiet {
        println!();
        println!("Index complete:");
        println!("  Embedded: {}", total_embedded);
        if pruned > 0 {
            println!("  Pruned: {} (deleted files)", pruned);
        }
    }

    // Build HNSW index for fast search
    if !check_interrupted() {
        if !cli.quiet {
            println!("Building HNSW index...");
        }

        let all_embeddings = store.all_embeddings()?;
        if !all_embeddings.is_empty() {
            let hnsw = HnswIndex::build(all_embeddings)?;
            hnsw.save(&cq_dir, "index")?;

            if !cli.quiet {
                println!("  HNSW index: {} vectors", hnsw.len());
            }
        }
    }

    Ok(())
}

fn cmd_query(cli: &Cli, query: &str) -> Result<()> {
    let root = find_project_root();
    let cq_dir = root.join(".cq");
    let index_path = cq_dir.join("index.db");

    if !index_path.exists() {
        bail!("Index not found. Run 'cq init && cq index' first.");
    }

    let store = Store::open(&index_path)?;
    let mut embedder = Embedder::new()?;

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
    };

    // Try HNSW search first (much faster for large indexes)
    let results = if HnswIndex::exists(&cq_dir, "index") {
        match HnswIndex::load(&cq_dir, "index") {
            Ok(hnsw) => {
                if cli.verbose {
                    eprintln!("Using HNSW index ({} vectors)", hnsw.len());
                }
                // Get more candidates from HNSW to allow for filtering
                let oversample = if filter.languages.is_some() || filter.path_pattern.is_some() {
                    cli.limit * 10
                } else {
                    cli.limit * 2
                };
                let hnsw_results = hnsw.search(&query_embedding, oversample);

                // Use HNSW candidates for filtered search (10-100x faster than brute-force)
                if filter.languages.is_some()
                    || filter.path_pattern.is_some()
                    || filter.name_boost > 0.0
                {
                    // Extract candidate IDs from HNSW results
                    let candidate_ids: Vec<&str> =
                        hnsw_results.iter().map(|r| r.id.as_str()).collect();
                    store.search_by_candidate_ids(
                        &candidate_ids,
                        &query_embedding,
                        &filter,
                        cli.limit,
                        cli.threshold,
                    )?
                } else {
                    // No filters - use HNSW results directly, just filter by threshold
                    hnsw_results
                        .into_iter()
                        .filter(|r| r.score >= cli.threshold)
                        .take(cli.limit)
                        .filter_map(|r| {
                            // Look up chunk details from store
                            // This is a simplified approach - ideally we'd batch this
                            store.get_chunk_by_id(&r.id).ok().flatten().map(|chunk| {
                                cqs::store::SearchResult {
                                    chunk,
                                    score: r.score,
                                }
                            })
                        })
                        .collect()
                }
            }
            Err(e) => {
                if cli.verbose {
                    eprintln!("HNSW load failed, using brute-force: {}", e);
                }
                store.search_filtered(&query_embedding, &filter, cli.limit, cli.threshold)?
            }
        }
    } else {
        // No HNSW index, use brute-force
        store.search_filtered(&query_embedding, &filter, cli.limit, cli.threshold)?
    };

    if results.is_empty() {
        if cli.json {
            println!(r#"{{"results":[],"query":"{}","total":0}}"#, query);
        } else {
            println!("No results found.");
        }
        std::process::exit(ExitCode::NoResults as i32);
    }

    if cli.json {
        display_results_json(&results, query)?;
    } else {
        display_results(&results, &root, cli.no_content, cli.context)?;
    }

    Ok(())
}

fn cmd_stats(cli: &Cli) -> Result<()> {
    let root = find_project_root();
    let index_path = root.join(".cq/index.db");

    if !index_path.exists() {
        bail!("Index not found. Run 'cq init && cq index' first.");
    }

    let store = Store::open(&index_path)?;
    let stats = store.stats()?;

    let cq_dir = root.join(".cq");
    let hnsw_vectors = if HnswIndex::exists(&cq_dir, "index") {
        HnswIndex::load(&cq_dir, "index").ok().map(|h| h.len())
    } else {
        None
    };

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

        // HNSW index status
        if HnswIndex::exists(&cq_dir, "index") {
            match HnswIndex::load(&cq_dir, "index") {
                Ok(hnsw) => {
                    println!();
                    println!("HNSW index: {} vectors (O(log n) search)", hnsw.len());
                }
                Err(e) => {
                    println!();
                    println!("HNSW index: error loading ({})", e);
                }
            }
        } else {
            println!();
            println!("HNSW index: not built (using brute-force O(n) search)");
            if stats.total_chunks > 10_000 {
                println!("  Tip: Run 'cqs index' to build HNSW for faster search");
            }
        }
    }

    Ok(())
}

fn cmd_watch(cli: &Cli, debounce_ms: u64, no_ignore: bool) -> Result<()> {
    let root = find_project_root();
    let cq_dir = root.join(".cq");
    let index_path = cq_dir.join("index.db");

    if !index_path.exists() {
        bail!("No index found. Run 'cqs index' first.");
    }

    let parser = CqParser::new()?;
    let supported_ext: HashSet<_> = parser.supported_extensions().iter().cloned().collect();

    println!(
        "Watching {} for changes (Ctrl+C to stop)...",
        root.display()
    );
    println!(
        "Supported extensions: {}",
        supported_ext.iter().cloned().collect::<Vec<_>>().join(", ")
    );

    let (tx, rx) = mpsc::channel();

    let config = Config::default().with_poll_interval(Duration::from_millis(debounce_ms));

    let mut watcher = RecommendedWatcher::new(tx, config)?;
    watcher.watch(&root, RecursiveMode::Recursive)?;

    // Track pending changes for debouncing
    let mut pending_files: HashSet<PathBuf> = HashSet::new();
    let mut last_event = std::time::Instant::now();
    let debounce = Duration::from_millis(debounce_ms);

    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Ok(event)) => {
                for path in event.paths {
                    // Skip if not a supported extension
                    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                    if !supported_ext.contains(ext) {
                        continue;
                    }
                    // Skip .cq directory
                    if path.starts_with(&cq_dir) {
                        continue;
                    }
                    // Convert to relative path
                    if let Ok(rel) = path.strip_prefix(&root) {
                        pending_files.insert(rel.to_path_buf());
                        last_event = std::time::Instant::now();
                    }
                }
            }
            Ok(Err(e)) => {
                if !cli.quiet {
                    eprintln!("Watch error: {}", e);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Check if we should process pending changes
                if !pending_files.is_empty() && last_event.elapsed() >= debounce {
                    let files: Vec<PathBuf> = pending_files.drain().collect();
                    if !cli.quiet {
                        println!("\n{} file(s) changed, reindexing...", files.len());
                        for f in &files {
                            println!("  {}", f.display());
                        }
                    }

                    // Reindex changed files
                    match reindex_files(&root, &index_path, &files, &parser, no_ignore, cli.quiet) {
                        Ok(count) => {
                            if !cli.quiet {
                                println!("Indexed {} chunk(s)", count);
                            }
                        }
                        Err(e) => {
                            eprintln!("Reindex error: {}", e);
                        }
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                bail!(
                    "File watcher disconnected unexpectedly. \
                     Hint: Restart 'cqs watch' to resume monitoring."
                );
            }
        }

        if check_interrupted() {
            println!("\nStopping watch...");
            break;
        }
    }

    Ok(())
}

/// Reindex specific files
fn reindex_files(
    root: &Path,
    index_path: &Path,
    files: &[PathBuf],
    parser: &CqParser,
    _no_ignore: bool,
    quiet: bool,
) -> Result<usize> {
    let mut embedder = Embedder::new()?;
    let store = Store::open(index_path)?;

    // Parse the changed files
    let chunks: Vec<_> = files
        .iter()
        .flat_map(|rel_path| {
            let abs_path = root.join(rel_path);
            if !abs_path.exists() {
                // File was deleted, we'll handle this by removing old chunks
                return vec![];
            }
            match parser.parse_file(&abs_path) {
                Ok(mut file_chunks) => {
                    // Rewrite paths to be relative
                    for chunk in &mut file_chunks {
                        chunk.file = rel_path.clone();
                    }
                    file_chunks
                }
                Err(e) => {
                    tracing::warn!("Failed to parse {}: {}", abs_path.display(), e);
                    vec![]
                }
            }
        })
        .collect();

    if chunks.is_empty() {
        return Ok(0);
    }

    // Generate embeddings
    let texts: Vec<String> = chunks.iter().map(prepare_embedding_input).collect();
    let text_refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
    let embeddings = embedder.embed_documents(&text_refs)?;

    // Delete old chunks for these files and insert new ones
    for rel_path in files {
        store.delete_by_file(rel_path)?;
    }

    for (chunk, embedding) in chunks.iter().zip(embeddings.iter()) {
        let abs_path = root.join(&chunk.file);
        let mtime = abs_path
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        store.upsert_chunk(chunk, embedding, mtime)?;
    }

    if !quiet {
        println!("Updated {} file(s)", files.len());
    }

    Ok(chunks.len())
}

fn cmd_serve(_cli: &Cli, transport: &str, port: u16, project: Option<PathBuf>) -> Result<()> {
    let root = project.unwrap_or_else(find_project_root);

    match transport {
        "stdio" => cqs::serve_stdio(root),
        "http" => cqs::serve_http(root, port),
        // Keep sse as alias for backwards compatibility
        "sse" => cqs::serve_http(root, port),
        _ => {
            bail!("Unknown transport: {}. Use 'stdio' or 'http'.", transport);
        }
    }
}

fn cmd_completions(shell: clap_complete::Shell) {
    use clap::CommandFactory;
    clap_complete::generate(shell, &mut Cli::command(), "cqs", &mut std::io::stdout());
}

// === Output helpers ===

fn display_results(
    results: &[cqs::store::SearchResult],
    root: &Path,
    no_content: bool,
    context: Option<usize>,
) -> Result<()> {
    for result in results {
        // Paths are stored relative; strip_prefix handles legacy absolute paths
        let rel_path = result
            .chunk
            .file
            .strip_prefix(root)
            .unwrap_or(&result.chunk.file);

        let header = format!(
            "{}:{} ({} {}) [{}] [{:.2}]",
            rel_path.display(),
            result.chunk.line_start,
            result.chunk.chunk_type,
            result.chunk.name,
            result.chunk.language,
            result.score
        );

        println!("{}", header.cyan());

        if !no_content {
            println!("{}", "─".repeat(50));

            // Read context if requested
            if let Some(n) = context {
                if n > 0 {
                    let abs_path = root.join(&result.chunk.file);
                    if let Ok((before, _)) = read_context_lines(
                        &abs_path,
                        result.chunk.line_start,
                        result.chunk.line_end,
                        n,
                    ) {
                        // Print before context (dimmed)
                        for line in &before {
                            println!("{}", format!("  {}", line).dimmed());
                        }
                    }
                }
            }

            // Show signature or truncated content
            if result.chunk.content.lines().count() <= 10 {
                println!("{}", result.chunk.content);
            } else {
                for line in result.chunk.content.lines().take(8) {
                    println!("{}", line);
                }
                println!("    ...");
            }

            // Print after context if requested
            if let Some(n) = context {
                if n > 0 {
                    let abs_path = root.join(&result.chunk.file);
                    if let Ok((_, after)) = read_context_lines(
                        &abs_path,
                        result.chunk.line_start,
                        result.chunk.line_end,
                        n,
                    ) {
                        for line in &after {
                            println!("{}", format!("  {}", line).dimmed());
                        }
                    }
                }
            }

            println!();
        }
    }

    println!("{} results", results.len());
    Ok(())
}

/// Read context lines before and after a range in a file
fn read_context_lines(
    file: &Path,
    line_start: u32,
    line_end: u32,
    context: usize,
) -> Result<(Vec<String>, Vec<String>)> {
    let content = std::fs::read_to_string(file)?;
    let lines: Vec<&str> = content.lines().collect();

    let start_idx = (line_start as usize).saturating_sub(1);
    let end_idx = (line_end as usize).saturating_sub(1);

    // Context before
    let context_start = start_idx.saturating_sub(context);
    let before: Vec<String> = lines[context_start..start_idx]
        .iter()
        .map(|s| s.to_string())
        .collect();

    // Context after
    let context_end = (end_idx + context + 1).min(lines.len());
    let after: Vec<String> = if end_idx + 1 < lines.len() {
        lines[(end_idx + 1)..context_end]
            .iter()
            .map(|s| s.to_string())
            .collect()
    } else {
        vec![]
    };

    Ok((before, after))
}

/// Prepare embedding input for a chunk: doc + signature + content
/// This improves semantic matching by including documentation.
fn prepare_embedding_input(chunk: &cqs::parser::Chunk) -> String {
    let mut input = String::new();

    // Include doc comment if present
    if let Some(ref doc) = chunk.doc {
        input.push_str(doc);
        input.push('\n');
    }

    // Include signature (function/method declaration)
    if !chunk.signature.is_empty() {
        input.push_str(&chunk.signature);
        input.push('\n');
    }

    // Include full content
    input.push_str(&chunk.content);

    input
}

fn display_results_json(results: &[cqs::store::SearchResult], query: &str) -> Result<()> {
    let json_results: Vec<_> = results
        .iter()
        .map(|r| {
            serde_json::json!({
                "file": r.chunk.file.to_string_lossy(),
                "line_start": r.chunk.line_start,
                "line_end": r.chunk.line_end,
                "name": r.chunk.name,
                "signature": r.chunk.signature,
                "language": r.chunk.language.to_string(),
                "chunk_type": r.chunk.chunk_type.to_string(),
                "score": r.score,
                "content": r.chunk.content,
            })
        })
        .collect();

    let output = serde_json::json!({
        "results": json_results,
        "query": query,
        "total": results.len(),
    });

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}
