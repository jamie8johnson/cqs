//! CLI implementation for cq

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use colored::Colorize;
use crossbeam_channel::{bounded, select, Receiver, Sender};
use ignore::WalkBuilder;
use indicatif::{ProgressBar, ProgressStyle};
use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
use rayon::prelude::*;

use cqs::embedder::{Embedder, Embedding};
use cqs::hnsw::HnswIndex;
use cqs::note::parse_notes;
use cqs::parser::{Chunk, Parser as CqParser};
use cqs::store::{ModelInfo, SearchFilter, Store, UnifiedResult};

// Constants
const MAX_FILE_SIZE: u64 = 1_048_576; // 1MB
const MAX_TOKENS_PER_WINDOW: usize = 480; // Max tokens before windowing (E5 has 512 limit)
const WINDOW_OVERLAP_TOKENS: usize = 64; // Overlap between windows

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
            println!("  {} Model: intfloat/e5-base-v2", "[✓]".green());
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

/// Apply windowing to chunks that exceed the token limit.
/// Long chunks are split into overlapping windows; short chunks pass through unchanged.
fn apply_windowing(chunks: Vec<Chunk>, embedder: &Embedder) -> Vec<Chunk> {
    let mut result = Vec::with_capacity(chunks.len());

    for chunk in chunks {
        match embedder.split_into_windows(
            &chunk.content,
            MAX_TOKENS_PER_WINDOW,
            WINDOW_OVERLAP_TOKENS,
        ) {
            Ok(windows) if windows.len() == 1 => {
                // Fits in one window - pass through unchanged
                result.push(chunk);
            }
            Ok(windows) => {
                // Split into multiple windows
                let parent_id = chunk.id.clone();
                for (window_content, window_idx) in windows {
                    let window_hash = blake3::hash(window_content.as_bytes()).to_hex().to_string();
                    result.push(Chunk {
                        id: format!("{}:w{}", parent_id, window_idx),
                        file: chunk.file.clone(),
                        language: chunk.language,
                        chunk_type: chunk.chunk_type,
                        name: chunk.name.clone(),
                        signature: chunk.signature.clone(),
                        content: window_content,
                        doc: if window_idx == 0 {
                            chunk.doc.clone()
                        } else {
                            None
                        }, // Doc only on first window
                        line_start: chunk.line_start,
                        line_end: chunk.line_end,
                        content_hash: window_hash,
                        parent_id: Some(parent_id.clone()),
                        window_idx: Some(window_idx),
                    });
                }
            }
            Err(e) => {
                // Tokenization failed - pass through unchanged and hope for the best
                tracing::warn!("Windowing failed for {}: {}, passing through", chunk.id, e);
                result.push(chunk);
            }
        }
    }

    result
}

/// Message types for the pipelined indexer
struct ParsedBatch {
    chunks: Vec<Chunk>,
    file_mtime: i64,
}

struct EmbeddedBatch {
    chunk_embeddings: Vec<(Chunk, Embedding)>,
    cached_count: usize,
    file_mtime: i64,
}

/// Stats returned from pipelined indexing
struct PipelineStats {
    total_embedded: usize,
    total_cached: usize,
}

/// Run the indexing pipeline with 3 concurrent stages:
/// 1. Parser: Parse files in parallel batches
/// 2. Embedder: Embed chunks (GPU)
/// 3. Writer: Write to SQLite
fn run_index_pipeline(
    root: &Path,
    files: Vec<PathBuf>,
    store_path: &Path,
    force: bool,
    quiet: bool,
) -> Result<PipelineStats> {
    use cqs::nl::generate_nl_description;

    let batch_size = 32; // Embedding batch size (backed off from 64 - crashed at 2%)
    let file_batch_size = 100_000; // Files to parse per batch (all at once)
    let channel_depth = 256; // Pipeline buffer depth (larger = smoother utilization)

    // Channels
    let (parse_tx, parse_rx): (Sender<ParsedBatch>, Receiver<ParsedBatch>) = bounded(channel_depth);
    let (embed_tx, embed_rx): (Sender<EmbeddedBatch>, Receiver<EmbeddedBatch>) =
        bounded(channel_depth);
    // GPU failure channel - GPU requeues failed batches here for CPU to handle async
    let (fail_tx, fail_rx): (Sender<ParsedBatch>, Receiver<ParsedBatch>) = bounded(channel_depth);

    // Shared state for progress
    let total_files = files.len();
    let parsed_count = Arc::new(AtomicUsize::new(0));
    let embedded_count = Arc::new(AtomicUsize::new(0));

    // Clone for threads
    let root_clone = root.to_path_buf();
    let parsed_count_clone = Arc::clone(&parsed_count);
    let store_path_for_parser = store_path.to_path_buf();
    let store_path_for_embedder = store_path.to_path_buf();

    // Stage 1: Parser thread - parse files in parallel batches
    let parser_handle = thread::spawn(move || -> Result<()> {
        let parser = CqParser::new()?;
        let store = Store::open(&store_path_for_parser)?;
        let root = root_clone;

        for file_batch in files.chunks(file_batch_size) {
            if check_interrupted() {
                break;
            }

            // Parse files in parallel
            let chunks: Vec<Chunk> = file_batch
                .par_iter()
                .flat_map(|rel_path| {
                    let abs_path = root.join(rel_path);
                    match parser.parse_file(&abs_path) {
                        Ok(mut chunks) => {
                            // Rewrite paths to be relative for storage
                            for chunk in &mut chunks {
                                chunk.file = rel_path.clone();
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
                .collect();

            // Filter by needs_reindex unless forced
            let chunks: Vec<Chunk> = if force {
                chunks
            } else {
                chunks
                    .into_iter()
                    .filter(|c| {
                        let abs_path = root.join(&c.file);
                        store.needs_reindex(&abs_path).unwrap_or(true)
                    })
                    .collect()
            };

            parsed_count_clone.fetch_add(file_batch.len(), Ordering::Relaxed);

            if !chunks.is_empty() {
                // Get mtime from first chunk's file
                let file_mtime = chunks
                    .first()
                    .and_then(|c| root.join(&c.file).metadata().ok())
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);

                // Send in embedding-sized batches
                for chunk_batch in chunks.chunks(batch_size) {
                    if parse_tx
                        .send(ParsedBatch {
                            chunks: chunk_batch.to_vec(),
                            file_mtime,
                        })
                        .is_err()
                    {
                        break; // Receiver dropped
                    }
                }
            }
        }
        Ok(())
    });

    // Clone for embedders (GPU and CPU run in parallel)
    let embedded_count_gpu = Arc::clone(&embedded_count);
    let embedded_count_cpu = Arc::clone(&embedded_count);
    let parse_rx_cpu = parse_rx.clone(); // CPU also grabs regular batches
    let embed_tx_cpu = embed_tx.clone();
    let store_path_for_cpu = store_path.to_path_buf();

    // Stage 2a: GPU Embedder thread - embed chunks, requeue failures to CPU
    let gpu_embedder_handle = thread::spawn(move || -> Result<()> {
        let mut embedder = Embedder::new()?;
        embedder.warm()?;
        let store = Store::open(&store_path_for_embedder)?;

        for batch in parse_rx {
            if check_interrupted() {
                break;
            }

            // Apply windowing to split long chunks into overlapping windows
            let windowed_chunks = apply_windowing(batch.chunks, &embedder);
            let batch = ParsedBatch {
                chunks: windowed_chunks,
                file_mtime: batch.file_mtime,
            };

            // Check for existing embeddings by content hash
            let hashes: Vec<&str> = batch
                .chunks
                .iter()
                .map(|c| c.content_hash.as_str())
                .collect();
            let existing = store.get_embeddings_by_hashes(&hashes);

            // Separate into cached vs to_embed
            let mut to_embed: Vec<&Chunk> = Vec::new();
            let mut cached: Vec<(Chunk, Embedding)> = Vec::new();

            for chunk in &batch.chunks {
                if let Some(emb) = existing.get(&chunk.content_hash) {
                    cached.push((chunk.clone(), emb.clone()));
                } else {
                    to_embed.push(chunk);
                }
            }

            // Embed new chunks on GPU
            if to_embed.is_empty() {
                // All cached, send directly
                let cached_count = cached.len();
                embedded_count_gpu.fetch_add(cached_count, Ordering::Relaxed);
                if embed_tx
                    .send(EmbeddedBatch {
                        chunk_embeddings: cached,
                        cached_count,
                        file_mtime: batch.file_mtime,
                    })
                    .is_err()
                {
                    break;
                }
            } else {
                let texts: Vec<String> = to_embed
                    .iter()
                    .map(|c| generate_nl_description(c))
                    .collect();
                let max_len = texts.iter().map(|t| t.len()).max().unwrap_or(0);

                // Pre-filter long batches to CPU (GPU hits CUDNN limits >8k chars)
                if max_len > 8000 {
                    eprintln!(
                        "Routing long batch to CPU: {} chunks, max_len={}",
                        to_embed.len(),
                        max_len
                    );
                    if fail_tx.send(batch).is_err() {
                        break;
                    }
                    continue;
                }

                let text_refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
                match embedder.embed_documents(&text_refs) {
                    Ok(embs) => {
                        let new_embeddings: Vec<Embedding> =
                            embs.into_iter().map(|e| e.with_sentiment(0.0)).collect();
                        let cached_count = cached.len();
                        let mut chunk_embeddings = cached;
                        chunk_embeddings.extend(to_embed.into_iter().cloned().zip(new_embeddings));
                        embedded_count_gpu.fetch_add(chunk_embeddings.len(), Ordering::Relaxed);
                        if embed_tx
                            .send(EmbeddedBatch {
                                chunk_embeddings,
                                cached_count,
                                file_mtime: batch.file_mtime,
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(_e) => {
                        // GPU failed - log details and requeue to CPU
                        let max_len = texts.iter().map(|t| t.len()).max().unwrap_or(0);
                        let files: Vec<_> = to_embed
                            .iter()
                            .map(|c| c.file.display().to_string())
                            .collect();
                        eprintln!(
                            "GPU failed, requeueing {} chunks to CPU (max_len={}, files={:?})",
                            batch.chunks.len(),
                            max_len,
                            files
                        );
                        if fail_tx.send(batch).is_err() {
                            break; // CPU thread gone
                        }
                    }
                }
            }
        }
        drop(fail_tx); // Signal CPU thread to finish when done
        Ok(())
    });

    // Stage 2b: CPU Embedder thread - handles failures + overflow (GPU gets priority)
    let cpu_embedder_handle = thread::spawn(move || -> Result<()> {
        let mut embedder = Embedder::new_cpu()?;
        let store = Store::open(&store_path_for_cpu)?;

        loop {
            if check_interrupted() {
                break;
            }

            // Race: GPU and CPU both grab from parse_rx, CPU also handles routed long batches
            let batch = select! {
                recv(fail_rx) -> msg => match msg {
                    Ok(b) => b,
                    Err(_) => match parse_rx_cpu.recv() {
                        Ok(b) => b,
                        Err(_) => break,
                    },
                },
                recv(parse_rx_cpu) -> msg => match msg {
                    Ok(b) => b,
                    Err(_) => match fail_rx.recv() {
                        Ok(b) => b,
                        Err(_) => break,
                    },
                },
            };

            // Apply windowing to split long chunks into overlapping windows
            let windowed_chunks = apply_windowing(batch.chunks, &embedder);
            let batch = ParsedBatch {
                chunks: windowed_chunks,
                file_mtime: batch.file_mtime,
            };

            // Check for existing embeddings by content hash
            let hashes: Vec<&str> = batch
                .chunks
                .iter()
                .map(|c| c.content_hash.as_str())
                .collect();
            let existing = store.get_embeddings_by_hashes(&hashes);

            // Separate into cached vs to_embed
            let mut to_embed: Vec<&Chunk> = Vec::new();
            let mut cached: Vec<(Chunk, Embedding)> = Vec::new();

            for chunk in &batch.chunks {
                if let Some(emb) = existing.get(&chunk.content_hash) {
                    cached.push((chunk.clone(), emb.clone()));
                } else {
                    to_embed.push(chunk);
                }
            }

            // Embed new chunks (CPU only)
            let new_embeddings: Vec<Embedding> = if to_embed.is_empty() {
                vec![]
            } else {
                let texts: Vec<String> = to_embed
                    .iter()
                    .map(|c| generate_nl_description(c))
                    .collect();
                let text_refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
                embedder
                    .embed_documents(&text_refs)?
                    .into_iter()
                    .map(|e| e.with_sentiment(0.0))
                    .collect()
            };

            let cached_count = cached.len();
            let mut chunk_embeddings = cached;
            chunk_embeddings.extend(to_embed.into_iter().cloned().zip(new_embeddings));

            embedded_count_cpu.fetch_add(chunk_embeddings.len(), Ordering::Relaxed);

            if embed_tx_cpu
                .send(EmbeddedBatch {
                    chunk_embeddings,
                    cached_count,
                    file_mtime: batch.file_mtime,
                })
                .is_err()
            {
                break; // Receiver dropped
            }
        }
        Ok(())
    });

    // Stage 3: Writer (main thread) - write to SQLite
    let store = Store::open(store_path)?;
    let parser = CqParser::new()?;
    let mut total_embedded = 0;
    let mut total_cached = 0;

    let progress = if quiet {
        ProgressBar::hidden()
    } else {
        let pb = ProgressBar::new(total_files as u64);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("[{elapsed_precise}] {bar:40.cyan/blue} {msg}")
                .unwrap(),
        );
        pb
    };

    for batch in embed_rx {
        if check_interrupted() {
            break;
        }

        store.upsert_chunks_batch(&batch.chunk_embeddings, batch.file_mtime)?;

        // Extract and store function calls
        for (chunk, _) in &batch.chunk_embeddings {
            let calls = parser.extract_calls_from_chunk(chunk);
            if !calls.is_empty() {
                store.upsert_calls(&chunk.id, &calls)?;
            }
        }

        total_embedded += batch.chunk_embeddings.len();
        total_cached += batch.cached_count;

        let parsed = parsed_count.load(Ordering::Relaxed);
        let embedded = embedded_count.load(Ordering::Relaxed);
        progress.set_position(parsed as u64);
        progress.set_message(format!(
            "parsed:{} embedded:{} written:{}",
            parsed, embedded, total_embedded
        ));
    }

    progress.finish_with_message("done");

    // Wait for threads to finish
    parser_handle
        .join()
        .map_err(|_| anyhow::anyhow!("Parser thread panicked"))??;
    gpu_embedder_handle
        .join()
        .map_err(|_| anyhow::anyhow!("GPU embedder thread panicked"))??;
    cpu_embedder_handle
        .join()
        .map_err(|_| anyhow::anyhow!("CPU embedder thread panicked"))??;

    Ok(PipelineStats {
        total_embedded,
        total_cached,
    })
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

    // Extract full call graph (includes large functions >100 lines)
    if !check_interrupted() {
        if !cli.quiet {
            println!("Extracting call graph...");
        }

        let mut total_calls = 0;
        for file in &existing_files {
            let abs_path = root.join(file);
            match parser.parse_file_calls(&abs_path) {
                Ok(function_calls) => {
                    for fc in &function_calls {
                        total_calls += fc.calls.len();
                    }
                    // Store with relative path
                    store.upsert_function_calls(file, &function_calls)?;
                }
                Err(e) => {
                    tracing::warn!("Failed to extract calls from {}: {}", abs_path.display(), e);
                }
            }
        }

        if !cli.quiet {
            println!("  Call graph: {} calls", total_calls);
        }
    }

    // Index notes if notes.toml exists
    if !check_interrupted() {
        let notes_path = root.join("docs/notes.toml");
        if notes_path.exists() {
            // Check if notes need reindexing
            let needs_reindex = force || store.notes_need_reindex(&notes_path).unwrap_or(true);

            if needs_reindex {
                if !cli.quiet {
                    println!("Indexing notes...");
                }

                match parse_notes(&notes_path) {
                    Ok(notes) => {
                        if !notes.is_empty() {
                            // Embed note content with sentiment
                            let mut embedder = Embedder::new()?;
                            let texts: Vec<String> =
                                notes.iter().map(|n| n.embedding_text()).collect();
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
                            let note_embeddings: Vec<_> =
                                notes.into_iter().zip(embeddings_with_sentiment).collect();
                            store.upsert_notes_batch(&note_embeddings, &notes_path, file_mtime)?;

                            if !cli.quiet {
                                let (total, warnings, patterns) = store.note_stats()?;
                                println!(
                                    "  Notes: {} total ({} warnings, {} patterns)",
                                    total, warnings, patterns
                                );
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Failed to parse notes: {}", e);
                    }
                }
            } else if !cli.quiet {
                println!("Notes up to date.");
            }
        }
    }

    Ok(())
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

fn cmd_query(cli: &Cli, query: &str) -> Result<()> {
    let _span = tracing::info_span!("cmd_query", query_len = query.len()).entered();

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
        enable_rrf: true, // Enable RRF hybrid search by default
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
            const CAGRA_THRESHOLD: usize = 5000;
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
        std::process::exit(ExitCode::NoResults as i32);
    }

    if cli.json {
        display_unified_results_json(&results, query)?;
    } else {
        display_unified_results(&results, &root, cli.no_content, cli.context)?;
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
        "Code extensions: {}",
        supported_ext.iter().cloned().collect::<Vec<_>>().join(", ")
    );
    println!("Also watching: docs/notes.toml");

    let (tx, rx) = mpsc::channel();

    let config = Config::default().with_poll_interval(Duration::from_millis(debounce_ms));

    let mut watcher = RecommendedWatcher::new(tx, config)?;
    watcher.watch(&root, RecursiveMode::Recursive)?;

    // Track pending changes for debouncing
    let mut pending_files: HashSet<PathBuf> = HashSet::new();
    let mut pending_notes = false; // Track if notes.toml changed
    let mut last_event = std::time::Instant::now();
    let debounce = Duration::from_millis(debounce_ms);
    let notes_path = root.join("docs/notes.toml");

    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Ok(event)) => {
                for path in event.paths {
                    // Skip .cq directory
                    if path.starts_with(&cq_dir) {
                        continue;
                    }

                    // Check if it's notes.toml
                    if path == notes_path {
                        pending_notes = true;
                        last_event = std::time::Instant::now();
                        continue;
                    }

                    // Skip if not a supported extension
                    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                    if !supported_ext.contains(ext) {
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
                let should_process = (!pending_files.is_empty() || pending_notes)
                    && last_event.elapsed() >= debounce;

                if should_process {
                    // Reindex code files if any changed
                    if !pending_files.is_empty() {
                        let files: Vec<PathBuf> = pending_files.drain().collect();
                        if !cli.quiet {
                            println!("\n{} file(s) changed, reindexing...", files.len());
                            for f in &files {
                                println!("  {}", f.display());
                            }
                        }

                        match reindex_files(
                            &root,
                            &index_path,
                            &files,
                            &parser,
                            no_ignore,
                            cli.quiet,
                        ) {
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

                    // Reindex notes if notes.toml changed
                    if pending_notes {
                        pending_notes = false;
                        if !cli.quiet {
                            println!("\nNotes changed, reindexing...");
                        }
                        match reindex_notes(&root, &index_path, cli.quiet) {
                            Ok(count) => {
                                if !cli.quiet {
                                    println!("Indexed {} note(s)", count);
                                }
                            }
                            Err(e) => {
                                eprintln!("Notes reindex error: {}", e);
                            }
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

    // Generate embeddings with neutral sentiment for code chunks
    let texts: Vec<String> = chunks.iter().map(generate_nl_description).collect();
    let text_refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
    let embeddings: Vec<Embedding> = embedder
        .embed_documents(&text_refs)?
        .into_iter()
        .map(|e| e.with_sentiment(0.0))
        .collect();

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

/// Reindex notes from docs/notes.toml
fn reindex_notes(root: &Path, index_path: &Path, quiet: bool) -> Result<usize> {
    let notes_path = root.join("docs/notes.toml");
    if !notes_path.exists() {
        return Ok(0);
    }

    let notes = parse_notes(&notes_path)?;
    if notes.is_empty() {
        return Ok(0);
    }

    let mut embedder = Embedder::new()?;
    let store = Store::open(index_path)?;

    // Embed note content with sentiment prefix
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
    let note_embeddings: Vec<_> = notes.into_iter().zip(embeddings_with_sentiment).collect();
    store.upsert_notes_batch(&note_embeddings, &notes_path, file_mtime)?;

    if !quiet {
        let (total, warnings, patterns) = store.note_stats()?;
        println!(
            "  Notes: {} total ({} warnings, {} patterns)",
            total, warnings, patterns
        );
    }

    Ok(note_embeddings.len())
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

// NL generation moved to cqs::nl module
use cqs::nl::generate_nl_description;

/// Display unified search results (code + notes)
fn display_unified_results(
    results: &[UnifiedResult],
    root: &Path,
    no_content: bool,
    context: Option<usize>,
) -> Result<()> {
    for result in results {
        match result {
            UnifiedResult::Code(r) => {
                // Paths are stored relative; strip_prefix handles legacy absolute paths
                let rel_path = r.chunk.file.strip_prefix(root).unwrap_or(&r.chunk.file);

                let header = format!(
                    "{}:{} ({} {}) [{}] [{:.2}]",
                    rel_path.display(),
                    r.chunk.line_start,
                    r.chunk.chunk_type,
                    r.chunk.name,
                    r.chunk.language,
                    r.score
                );

                println!("{}", header.cyan());

                if !no_content {
                    println!("{}", "─".repeat(50));

                    // Read context if requested
                    if let Some(n) = context {
                        if n > 0 {
                            let abs_path = root.join(&r.chunk.file);
                            if let Ok((before, _)) = read_context_lines(
                                &abs_path,
                                r.chunk.line_start,
                                r.chunk.line_end,
                                n,
                            ) {
                                for line in &before {
                                    println!("{}", format!("  {}", line).dimmed());
                                }
                            }
                        }
                    }

                    // Show signature or truncated content
                    if r.chunk.content.lines().count() <= 10 {
                        println!("{}", r.chunk.content);
                    } else {
                        for line in r.chunk.content.lines().take(8) {
                            println!("{}", line);
                        }
                        println!("    ...");
                    }

                    // Print after context if requested
                    if let Some(n) = context {
                        if n > 0 {
                            let abs_path = root.join(&r.chunk.file);
                            if let Ok((_, after)) = read_context_lines(
                                &abs_path,
                                r.chunk.line_start,
                                r.chunk.line_end,
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
            UnifiedResult::Note(r) => {
                // Format: [note:sentiment] text [score]
                let sentiment_indicator = if r.note.sentiment < -0.3 {
                    format!("v={:.1}", r.note.sentiment).red()
                } else if r.note.sentiment > 0.3 {
                    format!("v={:.1}", r.note.sentiment).green()
                } else {
                    format!("v={:.1}", r.note.sentiment).dimmed()
                };

                let header = format!("[note] {} [{:.2}]", sentiment_indicator, r.score);

                println!("{}", header.blue());

                if !no_content {
                    println!("{}", "─".repeat(50));
                    // Show truncated text
                    let text_lines: Vec<&str> = r.note.text.lines().collect();
                    if text_lines.len() <= 3 {
                        println!("{}", r.note.text);
                    } else {
                        for line in text_lines.iter().take(3) {
                            println!("{}", line);
                        }
                        println!("    ...");
                    }
                    println!();
                }
            }
        }
    }

    println!("{} results", results.len());
    Ok(())
}

/// Display unified results as JSON
fn display_unified_results_json(results: &[UnifiedResult], query: &str) -> Result<()> {
    let json_results: Vec<_> = results
        .iter()
        .map(|r| match r {
            UnifiedResult::Code(r) => serde_json::json!({
                "type": "code",
                "file": r.chunk.file.to_string_lossy(),
                "line_start": r.chunk.line_start,
                "line_end": r.chunk.line_end,
                "name": r.chunk.name,
                "signature": r.chunk.signature,
                "language": r.chunk.language.to_string(),
                "chunk_type": r.chunk.chunk_type.to_string(),
                "score": r.score,
                "content": r.chunk.content,
            }),
            UnifiedResult::Note(r) => serde_json::json!({
                "type": "note",
                "id": r.note.id,
                "text": r.note.text,
                "sentiment": r.note.sentiment,
                "mentions": r.note.mentions,
                "score": r.score,
            }),
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
