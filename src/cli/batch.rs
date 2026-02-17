//! Batch mode — persistent Store + Embedder, JSONL output
//!
//! Reads commands from stdin, executes against a shared Store and lazily-loaded
//! Embedder, outputs compact JSON per line. Amortizes ~100ms Store open and
//! ~500ms Embedder ONNX init across N commands.
//!
//! Supports pipeline syntax: `search "error" | callers | test-map` chains
//! commands where upstream names feed downstream commands via fan-out.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::OnceLock;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use cqs::index::VectorIndex;
use cqs::reference::ReferenceIndex;
use cqs::store::Store;
use cqs::Embedder;

use super::{open_project_store, DeadConfidenceLevel};

// ─── BatchContext ────────────────────────────────────────────────────────────

/// Shared resources for a batch session.
///
/// Store is opened once. Embedder and vector index are lazily initialized on
/// first use and cached for the session. References are cached per-name.
pub(crate) struct BatchContext {
    pub store: Store,
    embedder: OnceLock<Embedder>,
    hnsw: OnceLock<Option<Box<dyn VectorIndex>>>,
    refs: RefCell<HashMap<String, ReferenceIndex>>,
    pub root: PathBuf,
    pub cqs_dir: PathBuf,
    file_set: OnceLock<HashSet<PathBuf>>,
    audit_state: OnceLock<cqs::audit::AuditMode>,
    notes_cache: OnceLock<Vec<cqs::note::Note>>,
}

impl BatchContext {
    /// Get or create the embedder (~500ms first call).
    pub fn embedder(&self) -> Result<&Embedder> {
        if let Some(e) = self.embedder.get() {
            return Ok(e);
        }
        let _span = tracing::info_span!("batch_embedder_init").entered();
        let e = Embedder::new()?;
        // Race is fine — OnceLock ensures only one value is stored
        let _ = self.embedder.set(e);
        Ok(self.embedder.get().unwrap())
    }

    /// Get or build the vector index (CAGRA/HNSW/brute-force, cached).
    pub fn vector_index(&self) -> Result<Option<&dyn VectorIndex>> {
        if let Some(idx) = self.hnsw.get() {
            return Ok(idx.as_deref());
        }
        let _span = tracing::info_span!("batch_vector_index_init").entered();
        let idx = build_vector_index(&self.store, &self.cqs_dir)?;
        let _ = self.hnsw.set(idx);
        Ok(self.hnsw.get().unwrap().as_deref())
    }

    /// Get a cached reference index by name, loading on first access.
    pub fn get_ref(&self, name: &str) -> Result<()> {
        let refs = self.refs.borrow();
        if refs.contains_key(name) {
            return Ok(());
        }
        drop(refs);

        let config = cqs::config::Config::load(&self.root);
        let loaded = cqs::reference::load_references(&config.references);
        let found = loaded.into_iter().find(|r| r.name == name).ok_or_else(|| {
            anyhow::anyhow!(
                "Reference '{}' not found. Run 'cqs ref list' to see available references.",
                name
            )
        })?;
        self.refs.borrow_mut().insert(name.to_string(), found);
        Ok(())
    }

    /// Get or build the file set for staleness checks (cached).
    fn file_set(&self) -> Result<&HashSet<PathBuf>> {
        if let Some(fs) = self.file_set.get() {
            return Ok(fs);
        }
        let _span = tracing::info_span!("batch_file_set").entered();
        let exts: Vec<&str> = cqs::language::REGISTRY.supported_extensions().collect();
        let files = cqs::enumerate_files(&self.root, &exts, false)?;
        let set: HashSet<PathBuf> = files.into_iter().collect();
        let _ = self.file_set.set(set);
        Ok(self.file_set.get().unwrap())
    }

    /// Get cached audit state (loaded once per session).
    fn audit_state(&self) -> &cqs::audit::AuditMode {
        self.audit_state
            .get_or_init(|| cqs::audit::load_audit_state(&self.cqs_dir))
    }

    /// Get cached notes (parsed once per session).
    fn notes(&self) -> &[cqs::note::Note] {
        self.notes_cache.get_or_init(|| {
            let notes_path = self.root.join("docs/notes.toml");
            if notes_path.exists() {
                match cqs::note::parse_notes(&notes_path) {
                    Ok(notes) => notes,
                    Err(e) => {
                        tracing::warn!(error = %e, "Failed to parse notes.toml for batch");
                        vec![]
                    }
                }
            } else {
                vec![]
            }
        })
    }

    /// Borrow a reference index by name (must be loaded via `get_ref` first).
    pub fn borrow_ref(&self, name: &str) -> std::cell::Ref<'_, ReferenceIndex> {
        std::cell::Ref::map(self.refs.borrow(), |map| {
            map.get(name).expect("ref must be loaded via get_ref first")
        })
    }
}

/// Build the best available vector index for the store.
fn build_vector_index(
    store: &Store,
    cqs_dir: &std::path::Path,
) -> Result<Option<Box<dyn VectorIndex>>> {
    let _ = store; // Used only with gpu-search feature
    #[cfg(feature = "gpu-search")]
    {
        const CAGRA_THRESHOLD: u64 = 5000;
        let chunk_count = store.chunk_count().unwrap_or(0);
        if chunk_count >= CAGRA_THRESHOLD && cqs::CagraIndex::gpu_available() {
            match cqs::CagraIndex::build_from_store(store) {
                Ok(idx) => {
                    tracing::info!("Using CAGRA GPU index ({} vectors)", idx.len());
                    return Ok(Some(Box::new(idx) as Box<dyn VectorIndex>));
                }
                Err(e) => {
                    tracing::warn!("Failed to build CAGRA index, falling back to HNSW: {}", e);
                }
            }
        } else if chunk_count < CAGRA_THRESHOLD {
            tracing::debug!(
                "Index too small for CAGRA ({} < {}), using HNSW",
                chunk_count,
                CAGRA_THRESHOLD
            );
        } else {
            tracing::debug!("GPU not available, using HNSW");
        }
    }
    Ok(cqs::HnswIndex::try_load(cqs_dir))
}

// ─── BatchInput / BatchCmd ───────────────────────────────────────────────────

/// Parse a non-zero usize (reuse logic from CLI)
fn parse_nonzero_usize(s: &str) -> std::result::Result<usize, String> {
    let val: usize = s.parse().map_err(|e| format!("{e}"))?;
    if val == 0 {
        return Err("value must be at least 1".to_string());
    }
    Ok(val)
}

#[derive(Parser, Debug)]
#[command(
    no_binary_name = true,
    disable_help_subcommand = true,
    disable_help_flag = true
)]
pub(crate) struct BatchInput {
    #[command(subcommand)]
    pub cmd: BatchCmd,
}

#[derive(Subcommand, Debug)]
pub(crate) enum BatchCmd {
    /// Semantic search
    Search {
        /// Search query
        query: String,
        /// Max results
        #[arg(short = 'n', long, default_value = "5")]
        limit: usize,
        /// Definition search: find by name only
        #[arg(long)]
        name_only: bool,
        /// Pure semantic similarity, disable RRF hybrid
        #[arg(long)]
        semantic_only: bool,
        /// Re-rank results with cross-encoder
        #[arg(long)]
        rerank: bool,
        /// Filter by language
        #[arg(short = 'l', long)]
        lang: Option<String>,
        /// Filter by path pattern (glob)
        #[arg(short = 'p', long)]
        path: Option<String>,
        /// Maximum token budget
        #[arg(long, value_parser = parse_nonzero_usize)]
        tokens: Option<usize>,
    },
    /// Type dependencies: who uses a type, or what types a function uses
    Deps {
        /// Type name or function name
        name: String,
        /// Show types used by function (instead of type users)
        #[arg(long)]
        reverse: bool,
    },
    /// Find callers of a function
    Callers {
        /// Function name
        name: String,
    },
    /// Find callees of a function
    Callees {
        /// Function name
        name: String,
    },
    /// Function card: signature, callers, callees, similar
    Explain {
        /// Function name or file:function
        name: String,
        /// Maximum token budget
        #[arg(long, value_parser = parse_nonzero_usize)]
        tokens: Option<usize>,
    },
    /// Find similar code
    Similar {
        /// Function name or file:function
        target: String,
        /// Max results
        #[arg(short = 'n', long, default_value = "5")]
        limit: usize,
        /// Min similarity threshold
        #[arg(short = 't', long, default_value = "0.3")]
        threshold: f32,
    },
    /// Smart context assembly
    Gather {
        /// Search query
        query: String,
        /// Call graph expansion depth (0-5)
        #[arg(long, default_value = "1")]
        expand: usize,
        /// Direction: both, callers, callees
        #[arg(long, default_value = "both")]
        direction: String,
        /// Max chunks
        #[arg(short = 'n', long, default_value = "10")]
        limit: usize,
        /// Maximum token budget
        #[arg(long, value_parser = parse_nonzero_usize)]
        tokens: Option<usize>,
        /// Cross-index gather from reference
        #[arg(long = "ref")]
        ref_name: Option<String>,
    },
    /// Impact analysis
    Impact {
        /// Function name or file:function
        name: String,
        /// Caller depth (1=direct, 2+=transitive)
        #[arg(long, default_value = "1")]
        depth: usize,
        /// Suggest tests for untested callers
        #[arg(long)]
        suggest_tests: bool,
        /// Include type-impacted functions
        #[arg(long)]
        include_types: bool,
    },
    /// Map function to tests
    #[command(name = "test-map")]
    TestMap {
        /// Function name or file:function
        name: String,
        /// Max call chain depth
        #[arg(long, default_value = "5")]
        depth: usize,
    },
    /// Trace call path between two functions
    Trace {
        /// Source function
        source: String,
        /// Target function
        target: String,
        /// Max search depth
        #[arg(long, default_value = "10", value_parser = clap::value_parser!(u16).range(1..=50))]
        max_depth: u16,
    },
    /// Find dead code
    Dead {
        /// Include public API functions
        #[arg(long)]
        include_pub: bool,
        /// Minimum confidence level
        #[arg(long, default_value = "low")]
        min_confidence: DeadConfidenceLevel,
    },
    /// Find related functions by co-occurrence
    Related {
        /// Function name or file:function
        name: String,
        /// Max results per category
        #[arg(short = 'n', long, default_value = "5")]
        limit: usize,
    },
    /// Module-level context for a file
    Context {
        /// File path relative to project root
        path: String,
        /// Return summary counts
        #[arg(long)]
        summary: bool,
        /// Signatures-only TOC
        #[arg(long)]
        compact: bool,
        /// Maximum token budget
        #[arg(long, value_parser = parse_nonzero_usize)]
        tokens: Option<usize>,
    },
    /// Index statistics
    Stats,
    /// Pre-investigation dashboard
    Scout {
        /// Task description
        query: String,
        /// Max results
        #[arg(short = 'n', long, default_value = "10")]
        limit: usize,
        /// Maximum token budget
        #[arg(long, value_parser = parse_nonzero_usize)]
        tokens: Option<usize>,
    },
    /// Suggest where to add new code
    Where {
        /// Description of what to add
        description: String,
        /// Max suggestions
        #[arg(short = 'n', long, default_value = "5")]
        limit: usize,
    },
    /// Read file with note injection
    Read {
        /// File path relative to project root
        path: String,
        /// Focus on a specific function (focused read mode)
        #[arg(long)]
        focus: Option<String>,
    },
    /// Check index freshness
    Stale,
    /// Codebase quality snapshot
    Health,
    /// List notes
    Notes {
        /// Show only warnings (negative sentiment)
        #[arg(long)]
        warnings: bool,
        /// Show only patterns (positive sentiment)
        #[arg(long)]
        patterns: bool,
    },
    /// Show help
    Help,
}

// ─── Dispatch ────────────────────────────────────────────────────────────────

/// Execute a batch command and return a JSON value.
///
/// This is the seam for step 3 (REPL): import `BatchContext` + `dispatch`, wrap
/// with readline.
pub(crate) fn dispatch(ctx: &BatchContext, cmd: BatchCmd) -> Result<serde_json::Value> {
    match cmd {
        BatchCmd::Search {
            query,
            limit,
            name_only,
            semantic_only,
            rerank,
            lang,
            path,
            tokens,
        } => dispatch_search(
            ctx,
            &query,
            limit,
            name_only,
            semantic_only,
            rerank,
            lang,
            path,
            tokens,
        ),
        BatchCmd::Deps { name, reverse } => dispatch_deps(ctx, &name, reverse),
        BatchCmd::Callers { name } => dispatch_callers(ctx, &name),
        BatchCmd::Callees { name } => dispatch_callees(ctx, &name),
        BatchCmd::Explain { name, tokens } => dispatch_explain(ctx, &name, tokens),
        BatchCmd::Similar {
            target,
            limit,
            threshold,
        } => dispatch_similar(ctx, &target, limit, threshold),
        BatchCmd::Gather {
            query,
            expand,
            direction,
            limit,
            tokens,
            ref_name,
        } => dispatch_gather(
            ctx,
            &query,
            expand,
            &direction,
            limit,
            tokens,
            ref_name.as_deref(),
        ),
        BatchCmd::Impact {
            name,
            depth,
            suggest_tests,
            include_types,
        } => dispatch_impact(ctx, &name, depth, suggest_tests, include_types),
        BatchCmd::TestMap { name, depth } => dispatch_test_map(ctx, &name, depth),
        BatchCmd::Trace {
            source,
            target,
            max_depth,
        } => dispatch_trace(ctx, &source, &target, max_depth as usize),
        BatchCmd::Dead {
            include_pub,
            min_confidence,
        } => dispatch_dead(ctx, include_pub, &min_confidence),
        BatchCmd::Related { name, limit } => dispatch_related(ctx, &name, limit),
        BatchCmd::Context {
            path,
            summary,
            compact,
            tokens,
        } => dispatch_context(ctx, &path, summary, compact, tokens),
        BatchCmd::Stats => dispatch_stats(ctx),
        BatchCmd::Scout {
            query,
            limit,
            tokens,
        } => dispatch_scout(ctx, &query, limit, tokens),
        BatchCmd::Where { description, limit } => dispatch_where(ctx, &description, limit),
        BatchCmd::Read { path, focus } => dispatch_read(ctx, &path, focus.as_deref()),
        BatchCmd::Stale => dispatch_stale(ctx),
        BatchCmd::Health => dispatch_health(ctx),
        BatchCmd::Notes { warnings, patterns } => dispatch_notes(ctx, warnings, patterns),
        BatchCmd::Help => dispatch_help(),
    }
}

// ─── Handlers ────────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn dispatch_search(
    ctx: &BatchContext,
    query: &str,
    limit: usize,
    name_only: bool,
    semantic_only: bool,
    rerank: bool,
    lang: Option<String>,
    path: Option<String>,
    _tokens: Option<usize>,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_search", query).entered();

    if name_only {
        let results = ctx.store.search_by_name(query, limit.clamp(1, 100))?;
        let json_results: Vec<serde_json::Value> = results
            .iter()
            .map(|r| {
                serde_json::json!({
                    "name": r.chunk.name,
                    "file": r.chunk.file.to_string_lossy().replace('\\', "/"),
                    "line_start": r.chunk.line_start,
                    "line_end": r.chunk.line_end,
                    "language": r.chunk.language.to_string(),
                    "chunk_type": r.chunk.chunk_type.to_string(),
                    "signature": r.chunk.signature,
                    "score": r.score,
                })
            })
            .collect();
        return Ok(serde_json::json!({
            "results": json_results,
            "query": query,
            "total": json_results.len(),
        }));
    }

    let embedder = ctx.embedder()?;
    let query_embedding = embedder.embed_query(query)?;

    let languages = match &lang {
        Some(l) => Some(vec![l
            .parse()
            .map_err(|_| anyhow::anyhow!("Invalid language '{}'", l))?]),
        None => None,
    };

    let limit = limit.clamp(1, 100);
    let effective_limit = if rerank { (limit * 4).min(100) } else { limit };

    let filter = cqs::SearchFilter {
        languages,
        chunk_types: None,
        path_pattern: path,
        name_boost: 0.2,
        query_text: query.to_string(),
        enable_rrf: !semantic_only,
        note_weight: 1.0,
        note_only: false,
    };

    // Check audit mode
    let audit_mode = cqs::audit::load_audit_state(&ctx.cqs_dir);
    let index = ctx.vector_index()?;

    let results = if audit_mode.is_active() {
        let code_results = ctx.store.search_filtered_with_index(
            &query_embedding,
            &filter,
            effective_limit,
            0.3,
            index,
        )?;
        code_results
            .into_iter()
            .map(cqs::store::UnifiedResult::Code)
            .collect()
    } else {
        ctx.store.search_unified_with_index(
            &query_embedding,
            &filter,
            effective_limit,
            0.3,
            index,
        )?
    };

    // Re-rank if requested
    let results = if rerank && results.len() > 1 {
        let mut code_results = Vec::new();
        let mut note_results = Vec::new();
        for r in results {
            match r {
                cqs::store::UnifiedResult::Code(sr) => code_results.push(sr),
                note @ cqs::store::UnifiedResult::Note(_) => note_results.push(note),
            }
        }
        if code_results.len() > 1 {
            let reranker =
                cqs::Reranker::new().map_err(|e| anyhow::anyhow!("Reranker init failed: {e}"))?;
            reranker
                .rerank(query, &mut code_results, limit)
                .map_err(|e| anyhow::anyhow!("Reranking failed: {e}"))?;
        }
        let mut out: Vec<cqs::store::UnifiedResult> = code_results
            .into_iter()
            .map(cqs::store::UnifiedResult::Code)
            .collect();
        out.extend(note_results);
        out.truncate(limit);
        out
    } else {
        results
    };

    let json_results: Vec<serde_json::Value> = results
        .iter()
        .map(|r| match r {
            cqs::store::UnifiedResult::Code(sr) => serde_json::json!({
                "name": sr.chunk.name,
                "file": sr.chunk.file.to_string_lossy().replace('\\', "/"),
                "line_start": sr.chunk.line_start,
                "line_end": sr.chunk.line_end,
                "language": sr.chunk.language.to_string(),
                "chunk_type": sr.chunk.chunk_type.to_string(),
                "signature": sr.chunk.signature,
                "score": sr.score,
                "content": sr.chunk.content,
            }),
            cqs::store::UnifiedResult::Note(nr) => serde_json::json!({
                "type": "note",
                "text": nr.note.text,
                "score": nr.score,
                "sentiment": nr.note.sentiment,
            }),
        })
        .collect();

    Ok(serde_json::json!({
        "results": json_results,
        "query": query,
        "total": json_results.len(),
    }))
}

fn dispatch_deps(ctx: &BatchContext, name: &str, reverse: bool) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_deps", name, reverse).entered();

    if reverse {
        let types = ctx.store.get_types_used_by(name)?;
        Ok(serde_json::json!({
            "function": name,
            "types": types.iter().map(|(tn, kind)| {
                serde_json::json!({"type_name": tn, "edge_kind": kind})
            }).collect::<Vec<_>>(),
            "count": types.len(),
        }))
    } else {
        let users = ctx.store.get_type_users(name)?;
        let json_users: Vec<serde_json::Value> = users
            .iter()
            .map(|c| {
                serde_json::json!({
                    "name": c.name,
                    "file": cqs::rel_display(&c.file, &ctx.root),
                    "line_start": c.line_start,
                    "chunk_type": c.chunk_type.to_string(),
                })
            })
            .collect();
        Ok(serde_json::json!(json_users))
    }
}

fn dispatch_callers(ctx: &BatchContext, name: &str) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_callers", name).entered();
    let callers = ctx.store.get_callers_full(name)?;
    let json_callers: Vec<serde_json::Value> = callers
        .iter()
        .map(|c| {
            serde_json::json!({
                "name": c.name,
                "file": c.file.to_string_lossy().replace('\\', "/"),
                "line": c.line,
            })
        })
        .collect();
    Ok(serde_json::json!(json_callers))
}

fn dispatch_callees(ctx: &BatchContext, name: &str) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_callees", name).entered();
    let callees = ctx.store.get_callees_full(name, None)?;
    Ok(serde_json::json!({
        "function": name,
        "calls": callees.iter().map(|(n, line)| {
            serde_json::json!({"name": n, "line": line})
        }).collect::<Vec<_>>(),
        "count": callees.len(),
    }))
}

fn dispatch_explain(
    ctx: &BatchContext,
    target: &str,
    _tokens: Option<usize>,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_explain", target).entered();

    let resolved = cqs::resolve_target(&ctx.store, target)?;
    let chunk = &resolved.chunk;

    let callers = match ctx.store.get_callers_full(&chunk.name) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, name = chunk.name, "Failed to get callers in explain");
            vec![]
        }
    };
    let chunk_file = chunk.file.to_string_lossy();
    let callees = match ctx.store.get_callees_full(&chunk.name, Some(&chunk_file)) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, name = chunk.name, "Failed to get callees in explain");
            vec![]
        }
    };

    // Similar (top 3)
    let similar = match ctx.store.get_chunk_with_embedding(&chunk.id)? {
        Some((_, embedding)) => {
            let filter = cqs::SearchFilter {
                languages: None,
                chunk_types: None,
                path_pattern: None,
                name_boost: 0.0,
                query_text: String::new(),
                enable_rrf: false,
                note_weight: 0.0,
                note_only: false,
            };
            let index = ctx.vector_index()?;
            let sim_results = ctx
                .store
                .search_filtered_with_index(&embedding, &filter, 4, 0.3, index)?;
            sim_results
                .into_iter()
                .filter(|r| r.chunk.id != chunk.id)
                .take(3)
                .collect::<Vec<_>>()
        }
        None => vec![],
    };

    let callers_json: Vec<_> = callers
        .iter()
        .map(|c| {
            serde_json::json!({
                "name": c.name,
                "file": c.file.to_string_lossy().replace('\\', "/"),
                "line": c.line,
            })
        })
        .collect();

    let callees_json: Vec<_> = callees
        .iter()
        .map(|(name, line)| serde_json::json!({"name": name, "line": line}))
        .collect();

    let similar_json: Vec<_> = similar
        .iter()
        .map(|r| {
            serde_json::json!({
                "name": r.chunk.name,
                "file": r.chunk.file.to_string_lossy().replace('\\', "/"),
                "score": r.score,
            })
        })
        .collect();

    let rel_file = cqs::rel_display(&chunk.file, &ctx.root);

    Ok(serde_json::json!({
        "name": chunk.name,
        "file": rel_file,
        "language": chunk.language.to_string(),
        "chunk_type": chunk.chunk_type.to_string(),
        "lines": [chunk.line_start, chunk.line_end],
        "signature": chunk.signature,
        "doc": chunk.doc,
        "callers": callers_json,
        "callees": callees_json,
        "similar": similar_json,
    }))
}

fn dispatch_similar(
    ctx: &BatchContext,
    target: &str,
    limit: usize,
    threshold: f32,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_similar", target).entered();

    let resolved = cqs::resolve_target(&ctx.store, target)?;
    let chunk = &resolved.chunk;

    let (source_chunk, embedding) = ctx
        .store
        .get_chunk_with_embedding(&chunk.id)?
        .ok_or_else(|| anyhow::anyhow!("Could not load embedding for '{}'", chunk.name))?;

    let filter = cqs::SearchFilter {
        languages: None,
        chunk_types: None,
        path_pattern: None,
        name_boost: 0.0,
        query_text: String::new(),
        enable_rrf: false,
        note_weight: 0.0,
        note_only: false,
    };

    let index = ctx.vector_index()?;
    let results = ctx.store.search_filtered_with_index(
        &embedding,
        &filter,
        limit.saturating_add(1),
        threshold,
        index,
    )?;

    let filtered: Vec<_> = results
        .into_iter()
        .filter(|r| r.chunk.id != source_chunk.id)
        .take(limit)
        .collect();

    let json_results: Vec<serde_json::Value> = filtered
        .iter()
        .map(|r| {
            serde_json::json!({
                "name": r.chunk.name,
                "file": r.chunk.file.to_string_lossy().replace('\\', "/"),
                "score": r.score,
            })
        })
        .collect();

    Ok(serde_json::json!({
        "results": json_results,
        "target": chunk.name,
        "total": json_results.len(),
    }))
}

#[allow(clippy::too_many_arguments)]
fn dispatch_gather(
    ctx: &BatchContext,
    query: &str,
    expand: usize,
    direction: &str,
    limit: usize,
    _tokens: Option<usize>,
    ref_name: Option<&str>,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_gather", query, ?ref_name).entered();

    let embedder = ctx.embedder()?;
    let query_embedding = embedder.embed_query(query)?;

    let dir: cqs::GatherDirection = direction
        .parse()
        .map_err(|e: String| anyhow::anyhow!("{e}"))?;

    let opts = cqs::GatherOptions {
        expand_depth: expand.clamp(0, 5),
        direction: dir,
        limit,
        ..cqs::GatherOptions::default()
    };

    let result = if let Some(rn) = ref_name {
        ctx.get_ref(rn)?;
        let ref_idx = ctx.borrow_ref(rn);
        cqs::gather_cross_index(
            &ctx.store,
            &ref_idx,
            &query_embedding,
            query,
            &opts,
            &ctx.root,
        )?
    } else {
        cqs::gather(&ctx.store, &query_embedding, query, &opts, &ctx.root)?
    };

    let json_chunks: Vec<serde_json::Value> = result
        .chunks
        .iter()
        .map(|c| {
            let mut chunk_json = serde_json::json!({
                "name": c.name,
                "file": c.file.to_string_lossy().replace('\\', "/"),
                "line_start": c.line_start,
                "line_end": c.line_end,
                "language": c.language.to_string(),
                "chunk_type": c.chunk_type.to_string(),
                "signature": c.signature,
                "score": c.score,
                "depth": c.depth,
                "content": c.content,
            });
            if let Some(ref src) = c.source {
                chunk_json["source"] = serde_json::json!(src);
            }
            chunk_json
        })
        .collect();

    Ok(serde_json::json!({
        "query": query,
        "chunks": json_chunks,
        "expansion_capped": result.expansion_capped,
        "search_degraded": result.search_degraded,
    }))
}

fn dispatch_impact(
    ctx: &BatchContext,
    name: &str,
    depth: usize,
    do_suggest_tests: bool,
    include_types: bool,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_impact", name).entered();

    let resolved = cqs::resolve_target(&ctx.store, name)?;
    let chunk = &resolved.chunk;
    let depth = depth.clamp(1, 10);

    let result = cqs::analyze_impact(&ctx.store, &chunk.name, depth, include_types)?;

    let mut json = cqs::impact_to_json(&result, &ctx.root);

    if do_suggest_tests {
        let suggestions = cqs::suggest_tests(&ctx.store, &result);
        let suggestions_json: Vec<_> = suggestions
            .iter()
            .map(|s| {
                serde_json::json!({
                    "test_name": s.test_name,
                    "suggested_file": s.suggested_file,
                    "for_function": s.for_function,
                    "pattern_source": s.pattern_source,
                    "inline": s.inline,
                })
            })
            .collect();
        if let Some(obj) = json.as_object_mut() {
            obj.insert(
                "test_suggestions".into(),
                serde_json::json!(suggestions_json),
            );
        }
    }

    Ok(json)
}

fn dispatch_test_map(
    ctx: &BatchContext,
    name: &str,
    max_depth: usize,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_test_map", name).entered();

    let resolved = cqs::resolve_target(&ctx.store, name)?;
    let target_name = resolved.chunk.name.clone();

    let graph = ctx.store.get_call_graph()?;
    let test_chunks = ctx.store.find_test_chunks()?;

    // Reverse BFS from target
    let mut ancestors: HashMap<String, (usize, String)> = HashMap::new();
    let mut queue: std::collections::VecDeque<(String, usize)> = std::collections::VecDeque::new();
    ancestors.insert(target_name.clone(), (0, String::new()));
    queue.push_back((target_name.clone(), 0));

    while let Some((current, depth)) = queue.pop_front() {
        if depth >= max_depth {
            continue;
        }
        if let Some(callers) = graph.reverse.get(&current) {
            for caller in callers {
                if !ancestors.contains_key(caller) {
                    ancestors.insert(caller.clone(), (depth + 1, current.clone()));
                    queue.push_back((caller.clone(), depth + 1));
                }
            }
        }
    }

    struct TestMatch {
        name: String,
        file: String,
        line: u32,
        depth: usize,
        chain: Vec<String>,
    }

    let mut matches: Vec<TestMatch> = Vec::new();
    for test in &test_chunks {
        if let Some((depth, _)) = ancestors.get(&test.name) {
            if *depth > 0 {
                let mut chain = Vec::new();
                let mut current = test.name.clone();
                while !current.is_empty() {
                    chain.push(current.clone());
                    if current == target_name {
                        break;
                    }
                    current = ancestors
                        .get(&current)
                        .map(|(_, p)| p.clone())
                        .unwrap_or_default();
                }
                let rel_file = cqs::rel_display(&test.file, &ctx.root);
                matches.push(TestMatch {
                    name: test.name.clone(),
                    file: rel_file,
                    line: test.line_start,
                    depth: *depth,
                    chain,
                });
            }
        }
    }

    matches.sort_by(|a, b| a.depth.cmp(&b.depth).then_with(|| a.name.cmp(&b.name)));

    let tests_json: Vec<_> = matches
        .iter()
        .map(|m| {
            serde_json::json!({
                "name": m.name,
                "file": m.file,
                "line": m.line,
                "call_depth": m.depth,
                "call_chain": m.chain,
            })
        })
        .collect();

    Ok(serde_json::json!({
        "function": target_name,
        "tests": tests_json,
        "test_count": matches.len(),
    }))
}

fn dispatch_trace(
    ctx: &BatchContext,
    source: &str,
    target: &str,
    max_depth: usize,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_trace", source, target).entered();

    let source_resolved = cqs::resolve_target(&ctx.store, source)?;
    let target_resolved = cqs::resolve_target(&ctx.store, target)?;
    let source_name = source_resolved.chunk.name.clone();
    let target_name = target_resolved.chunk.name.clone();

    if source_name == target_name {
        let rel_file = cqs::rel_display(&source_resolved.chunk.file, &ctx.root);
        return Ok(serde_json::json!({
            "source": source_name,
            "target": target_name,
            "path": [{
                "name": source_name,
                "file": rel_file,
                "line": source_resolved.chunk.line_start,
                "signature": source_resolved.chunk.signature,
            }],
            "depth": 0,
        }));
    }

    let graph = ctx.store.get_call_graph()?;

    // BFS shortest path
    let mut visited: HashMap<String, String> = HashMap::new();
    let mut queue: std::collections::VecDeque<(String, usize)> = std::collections::VecDeque::new();
    visited.insert(source_name.clone(), String::new());
    queue.push_back((source_name.clone(), 0));
    let mut found_path: Option<Vec<String>> = None;

    while let Some((current, depth)) = queue.pop_front() {
        if current == target_name {
            let mut path = vec![current.clone()];
            let mut node = &current;
            while let Some(pred) = visited.get(node) {
                if pred.is_empty() {
                    break;
                }
                path.push(pred.clone());
                node = pred;
            }
            path.reverse();
            found_path = Some(path);
            break;
        }
        if depth >= max_depth {
            continue;
        }
        if let Some(callees) = graph.forward.get(&current) {
            for callee in callees {
                if !visited.contains_key(callee) {
                    visited.insert(callee.clone(), current.clone());
                    queue.push_back((callee.clone(), depth + 1));
                }
            }
        }
    }

    match found_path {
        Some(names) => {
            let mut path_json = Vec::new();
            for name in &names {
                let entry = match ctx.store.search_by_name(name, 1)?.into_iter().next() {
                    Some(r) => {
                        let rel = cqs::rel_display(&r.chunk.file, &ctx.root);
                        serde_json::json!({
                            "name": name,
                            "file": rel,
                            "line": r.chunk.line_start,
                            "signature": r.chunk.signature,
                        })
                    }
                    None => serde_json::json!({"name": name}),
                };
                path_json.push(entry);
            }

            Ok(serde_json::json!({
                "source": source_name,
                "target": target_name,
                "path": path_json,
                "depth": names.len() - 1,
            }))
        }
        None => Ok(serde_json::json!({
            "source": source_name,
            "target": target_name,
            "path": null,
            "message": format!("No call path found within depth {}", max_depth),
        })),
    }
}

fn dispatch_dead(
    ctx: &BatchContext,
    include_pub: bool,
    min_confidence: &DeadConfidenceLevel,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_dead").entered();

    let min_level: cqs::store::DeadConfidence = min_confidence.into();
    let (confident, possibly_pub) = ctx.store.find_dead_code(include_pub)?;

    let confident: Vec<_> = confident
        .into_iter()
        .filter(|d| d.confidence >= min_level)
        .collect();
    let possibly_pub: Vec<_> = possibly_pub
        .into_iter()
        .filter(|d| d.confidence >= min_level)
        .collect();

    let format_dead = |dead: &cqs::store::DeadFunction| {
        let confidence = match dead.confidence {
            cqs::store::DeadConfidence::High => "high",
            cqs::store::DeadConfidence::Medium => "medium",
            cqs::store::DeadConfidence::Low => "low",
        };
        serde_json::json!({
            "name": dead.chunk.name,
            "file": cqs::rel_display(&dead.chunk.file, &ctx.root),
            "line_start": dead.chunk.line_start,
            "line_end": dead.chunk.line_end,
            "chunk_type": dead.chunk.chunk_type.to_string(),
            "signature": dead.chunk.signature,
            "language": dead.chunk.language.to_string(),
            "confidence": confidence,
        })
    };

    Ok(serde_json::json!({
        "dead": confident.iter().map(&format_dead).collect::<Vec<_>>(),
        "possibly_dead_pub": possibly_pub.iter().map(&format_dead).collect::<Vec<_>>(),
        "total_dead": confident.len(),
        "total_possibly_dead_pub": possibly_pub.len(),
    }))
}

fn dispatch_related(ctx: &BatchContext, name: &str, limit: usize) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_related", name).entered();

    let result = cqs::find_related(&ctx.store, name, limit)?;

    let to_json = |items: &[cqs::RelatedFunction]| -> Vec<serde_json::Value> {
        items
            .iter()
            .map(|r| {
                let rel = cqs::rel_display(&r.file, &ctx.root);
                serde_json::json!({
                    "name": r.name,
                    "file": rel,
                    "line": r.line,
                    "overlap_count": r.overlap_count,
                })
            })
            .collect()
    };

    Ok(serde_json::json!({
        "target": result.target,
        "shared_callers": to_json(&result.shared_callers),
        "shared_callees": to_json(&result.shared_callees),
        "shared_types": to_json(&result.shared_types),
    }))
}

fn dispatch_context(
    ctx: &BatchContext,
    path: &str,
    summary: bool,
    compact: bool,
    _tokens: Option<usize>,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_context", path).entered();

    let abs_path = ctx.root.join(path);
    let origin = abs_path.to_string_lossy().to_string();

    let mut chunks = ctx.store.get_chunks_by_origin(&origin)?;
    if chunks.is_empty() {
        chunks = ctx.store.get_chunks_by_origin(path)?;
    }
    if chunks.is_empty() {
        anyhow::bail!(
            "No indexed chunks found for '{}'. Is the file indexed?",
            path
        );
    }

    if compact {
        let names: Vec<&str> = chunks.iter().map(|c| c.name.as_str()).collect();
        let caller_counts = ctx.store.get_caller_counts_batch(&names)?;
        let callee_counts = ctx.store.get_callee_counts_batch(&names)?;

        let entries: Vec<_> = chunks
            .iter()
            .map(|c| {
                let cc = caller_counts.get(&c.name).copied().unwrap_or(0);
                let ce = callee_counts.get(&c.name).copied().unwrap_or(0);
                serde_json::json!({
                    "name": c.name,
                    "chunk_type": c.chunk_type.to_string(),
                    "signature": c.signature,
                    "lines": [c.line_start, c.line_end],
                    "caller_count": cc,
                    "callee_count": ce,
                })
            })
            .collect();

        return Ok(serde_json::json!({
            "file": path,
            "chunks": entries,
            "total": entries.len(),
        }));
    }

    if summary {
        let names: Vec<&str> = chunks.iter().map(|c| c.name.as_str()).collect();
        let caller_counts = ctx.store.get_caller_counts_batch(&names)?;
        let callee_counts = ctx.store.get_callee_counts_batch(&names)?;
        let total_callers: u64 = caller_counts.values().sum();
        let total_callees: u64 = callee_counts.values().sum();

        return Ok(serde_json::json!({
            "file": path,
            "chunk_count": chunks.len(),
            "total_callers": total_callers,
            "total_callees": total_callees,
        }));
    }

    // Full context
    let entries: Vec<_> = chunks
        .iter()
        .map(|c| {
            serde_json::json!({
                "name": c.name,
                "chunk_type": c.chunk_type.to_string(),
                "language": c.language.to_string(),
                "lines": [c.line_start, c.line_end],
                "signature": c.signature,
                "content": c.content,
            })
        })
        .collect();

    Ok(serde_json::json!({
        "file": path,
        "chunks": entries,
        "total": entries.len(),
    }))
}

fn dispatch_stats(ctx: &BatchContext) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_stats").entered();
    let stats = ctx.store.stats()?;
    let note_count = ctx.store.note_count()?;
    let fc_stats = ctx.store.function_call_stats()?;
    let te_stats = ctx.store.type_edge_stats()?;

    Ok(serde_json::json!({
        "total_chunks": stats.total_chunks,
        "total_files": stats.total_files,
        "notes": note_count,
        "call_graph": {
            "total_calls": fc_stats.total_calls,
            "unique_callers": fc_stats.unique_callers,
            "unique_callees": fc_stats.unique_callees,
        },
        "type_graph": {
            "total_edges": te_stats.total_edges,
            "unique_types": te_stats.unique_types,
        },
        "by_language": stats.chunks_by_language.iter()
            .map(|(l, c)| (l.to_string(), c))
            .collect::<HashMap<String, _>>(),
        "by_type": stats.chunks_by_type.iter()
            .map(|(t, c)| (t.to_string(), c))
            .collect::<HashMap<String, _>>(),
        "model": stats.model_name,
        "schema_version": stats.schema_version,
    }))
}

fn dispatch_scout(
    ctx: &BatchContext,
    query: &str,
    limit: usize,
    _tokens: Option<usize>,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_scout", query).entered();
    let embedder = ctx.embedder()?;
    let limit = limit.clamp(1, 50);
    let result = cqs::scout(&ctx.store, embedder, query, &ctx.root, limit)?;
    Ok(cqs::scout_to_json(&result, &ctx.root))
}

fn dispatch_where(
    ctx: &BatchContext,
    description: &str,
    limit: usize,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_where", description).entered();
    let embedder = ctx.embedder()?;
    let limit = limit.clamp(1, 10);
    let result = cqs::suggest_placement(&ctx.store, embedder, description, limit)?;

    let suggestions_json: Vec<_> = result
        .suggestions
        .iter()
        .map(|s| {
            let rel = cqs::rel_display(&s.file, &ctx.root);
            serde_json::json!({
                "file": rel,
                "score": s.score,
                "insertion_line": s.insertion_line,
                "near_function": s.near_function,
                "reason": s.reason,
                "patterns": {
                    "imports": s.patterns.imports,
                    "error_handling": s.patterns.error_handling,
                    "naming_convention": s.patterns.naming_convention,
                    "visibility": s.patterns.visibility,
                    "has_inline_tests": s.patterns.has_inline_tests,
                }
            })
        })
        .collect();

    Ok(serde_json::json!({
        "description": description,
        "suggestions": suggestions_json,
    }))
}

fn dispatch_read(ctx: &BatchContext, path: &str, focus: Option<&str>) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_read", path).entered();

    // Focused read mode
    if let Some(focus) = focus {
        return dispatch_read_focused(ctx, focus);
    }

    let file_path = ctx.root.join(path);

    if !file_path.exists() {
        anyhow::bail!("File not found: {}", path);
    }

    // Path traversal protection
    let canonical = dunce::canonicalize(&file_path)
        .with_context(|| format!("Failed to canonicalize path: {}", path))?;
    let project_canonical =
        dunce::canonicalize(&ctx.root).context("Failed to canonicalize project root")?;
    if !canonical.starts_with(&project_canonical) {
        anyhow::bail!("Path traversal not allowed: {}", path);
    }

    // File size limit (10MB)
    const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024;
    let metadata = std::fs::metadata(&file_path).context("Failed to read file metadata")?;
    if metadata.len() > MAX_FILE_SIZE {
        anyhow::bail!(
            "File too large: {} bytes (max {} bytes)",
            metadata.len(),
            MAX_FILE_SIZE
        );
    }

    let content = std::fs::read_to_string(&file_path).context("Failed to read file")?;

    let audit_state = ctx.audit_state();
    let mut context_header = String::new();

    if let Some(status) = audit_state.status_line() {
        context_header.push_str(&format!("// {}\n//\n", status));
    }

    // Note injection (skip in audit mode)
    let mut notes_injected = false;
    if !audit_state.is_active() {
        let file_name = file_path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let relevant: Vec<_> = ctx
            .notes()
            .iter()
            .filter(|n| {
                n.mentions.iter().any(|m| {
                    m == file_name || m == path || cqs::note::path_matches_mention(path, m)
                })
            })
            .collect();

        if !relevant.is_empty() {
            notes_injected = true;
            context_header
                .push_str("// ┌─────────────────────────────────────────────────────────────┐\n");
            context_header
                .push_str("// │ [cqs] Context from notes.toml                              │\n");
            context_header
                .push_str("// └─────────────────────────────────────────────────────────────┘\n");
            for n in relevant {
                if let Some(first_line) = n.text.lines().next() {
                    context_header.push_str(&format!(
                        "// [{}] {}\n",
                        n.sentiment_label(),
                        first_line.trim()
                    ));
                }
            }
            context_header.push_str("//\n");
        }
    }

    let enriched = if context_header.is_empty() {
        content
    } else {
        format!("{}{}", context_header, content)
    };

    Ok(serde_json::json!({
        "path": path,
        "content": enriched,
        "notes_injected": notes_injected,
    }))
}

fn dispatch_read_focused(ctx: &BatchContext, focus: &str) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_read_focused", focus).entered();

    let resolved = cqs::resolve_target(&ctx.store, focus)?;
    let chunk = &resolved.chunk;
    let rel_file = cqs::rel_display(&chunk.file, &ctx.root);

    let mut output = String::new();
    output.push_str(&format!(
        "// [cqs] Focused read: {} ({}:{}-{})\n",
        chunk.name, rel_file, chunk.line_start, chunk.line_end
    ));

    // Hints
    let hints = if matches!(
        chunk.chunk_type,
        cqs::parser::ChunkType::Function | cqs::parser::ChunkType::Method
    ) {
        match cqs::compute_hints(&ctx.store, &chunk.name, None) {
            Ok(h) => Some(h),
            Err(e) => {
                tracing::warn!(function = %chunk.name, error = %e, "Failed to compute hints");
                None
            }
        }
    } else {
        None
    };
    if let Some(ref h) = hints {
        let caller_label = if h.caller_count == 0 {
            "! 0 callers".to_string()
        } else {
            format!("{} callers", h.caller_count)
        };
        let test_label = if h.test_count == 0 {
            "! 0 tests".to_string()
        } else {
            format!("{} tests", h.test_count)
        };
        output.push_str(&format!("// [cqs] {} | {}\n", caller_label, test_label));
    }

    // Audit mode status
    let audit_state = ctx.audit_state();
    if let Some(status) = audit_state.status_line() {
        output.push_str(&format!("// {}\n", status));
    }

    // Note injection (skip in audit mode)
    if !audit_state.is_active() {
        let relevant: Vec<_> = ctx
            .notes()
            .iter()
            .filter(|n| {
                n.mentions
                    .iter()
                    .any(|m| m == &chunk.name || m == &rel_file)
            })
            .collect();
        for n in &relevant {
            if let Some(first_line) = n.text.lines().next() {
                output.push_str(&format!(
                    "// [{}] {}\n",
                    n.sentiment_label(),
                    first_line.trim()
                ));
            }
        }
        if !relevant.is_empty() {
            output.push_str("//\n");
        }
    }

    // Target function
    output.push_str("\n// --- Target ---\n");
    if let Some(ref doc) = chunk.doc {
        output.push_str(doc);
        output.push('\n');
    }
    output.push_str(&chunk.content);
    output.push('\n');

    // Type dependencies
    let type_deps = match ctx.store.get_types_used_by(&chunk.name) {
        Ok(pairs) => pairs,
        Err(e) => {
            tracing::warn!(function = %chunk.name, error = %e, "Failed to query type deps");
            Vec::new()
        }
    };
    let mut seen_types = std::collections::HashSet::new();
    let filtered_types: Vec<(String, String)> = type_deps
        .into_iter()
        .filter(|(name, _kind)| !cqs::COMMON_TYPES.contains(name.as_str()))
        .filter(|(name, _kind)| seen_types.insert(name.clone()))
        .collect();

    for (type_name, edge_kind) in &filtered_types {
        if let Ok(results) = ctx.store.search_by_name(type_name, 5) {
            let type_def = results.iter().find(|r| {
                r.chunk.name == *type_name
                    && matches!(
                        r.chunk.chunk_type,
                        cqs::parser::ChunkType::Struct
                            | cqs::parser::ChunkType::Enum
                            | cqs::parser::ChunkType::Trait
                            | cqs::parser::ChunkType::Interface
                            | cqs::parser::ChunkType::Class
                    )
            });
            if let Some(r) = type_def {
                let dep_rel = cqs::rel_display(&r.chunk.file, &ctx.root);
                let kind_label = if edge_kind.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", edge_kind)
                };
                output.push_str(&format!(
                    "\n// --- Type: {}{} ({}:{}-{}) ---\n",
                    r.chunk.name, kind_label, dep_rel, r.chunk.line_start, r.chunk.line_end
                ));
                output.push_str(&r.chunk.content);
                output.push('\n');
            }
        }
    }

    let mut result = serde_json::json!({
        "focus": focus,
        "content": output,
    });
    if let Some(ref h) = hints {
        result["hints"] = serde_json::json!({
            "caller_count": h.caller_count,
            "test_count": h.test_count,
            "no_callers": h.caller_count == 0,
            "no_tests": h.test_count == 0,
        });
    }

    Ok(result)
}

fn dispatch_stale(ctx: &BatchContext) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_stale").entered();

    let file_set = ctx.file_set()?;
    let report = ctx.store.list_stale_files(file_set)?;

    let stale_json: Vec<_> = report
        .stale
        .iter()
        .map(|f| {
            serde_json::json!({
                "origin": f.origin,
                "stored_mtime": f.stored_mtime,
                "current_mtime": f.current_mtime,
            })
        })
        .collect();

    let missing_json: Vec<_> = report
        .missing
        .iter()
        .map(|origin| serde_json::json!(origin))
        .collect();

    Ok(serde_json::json!({
        "stale": stale_json,
        "missing": missing_json,
        "total_indexed": report.total_indexed,
        "stale_count": report.stale.len(),
        "missing_count": report.missing.len(),
    }))
}

fn dispatch_health(ctx: &BatchContext) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_health").entered();

    let file_set = ctx.file_set()?;
    let report = cqs::health::health_check(&ctx.store, file_set, &ctx.cqs_dir)?;

    let hotspots_json: Vec<_> = report
        .hotspots
        .iter()
        .map(|(name, count)| serde_json::json!({"name": name, "caller_count": count}))
        .collect();

    let untested_json: Vec<_> = report
        .untested_hotspots
        .iter()
        .map(|(name, count)| serde_json::json!({"name": name, "caller_count": count}))
        .collect();

    Ok(serde_json::json!({
        "stats": {
            "total_chunks": report.stats.total_chunks,
            "total_files": report.stats.total_files,
        },
        "stale_count": report.stale_count,
        "missing_count": report.missing_count,
        "dead_confident": report.dead_confident,
        "dead_possible": report.dead_possible,
        "hotspots": hotspots_json,
        "untested_hotspots": untested_json,
        "note_count": report.note_count,
        "note_warnings": report.note_warnings,
        "warnings": report.warnings,
    }))
}

fn dispatch_notes(ctx: &BatchContext, warnings: bool, patterns: bool) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_notes", warnings, patterns).entered();

    let notes = ctx.notes();
    let filtered: Vec<_> = notes
        .iter()
        .filter(|n| {
            if warnings {
                n.is_warning()
            } else if patterns {
                n.is_pattern()
            } else {
                true
            }
        })
        .map(|n| {
            serde_json::json!({
                "text": n.text,
                "sentiment": n.sentiment,
                "sentiment_label": n.sentiment_label(),
                "mentions": n.mentions,
            })
        })
        .collect();

    Ok(serde_json::json!({
        "notes": filtered,
        "total": filtered.len(),
    }))
}

fn dispatch_help() -> Result<serde_json::Value> {
    use clap::CommandFactory;
    let mut buf = Vec::new();
    BatchInput::command().write_help(&mut buf)?;
    let help_text = String::from_utf8_lossy(&buf).to_string();
    Ok(serde_json::json!({"help": help_text}))
}

// ─── Pipeline ─────────────────────────────────────────────────────────────────

/// Maximum names extracted per pipeline stage to prevent fan-out explosion.
/// A 3-stage pipeline dispatches at most 1 + 50 + 50 = 101 calls.
const PIPELINE_FAN_OUT_LIMIT: usize = 50;

/// Commands that accept a piped function name as their first positional arg.
const PIPEABLE_COMMANDS: &[&str] = &[
    "callers", "callees", "deps", "explain", "similar", "impact", "test-map", "related", "scout",
];

/// Check if a command (first token) can receive piped names.
fn is_pipeable_command(tokens: &[String]) -> bool {
    tokens
        .first()
        .map(|cmd| PIPEABLE_COMMANDS.contains(&cmd.as_str()))
        .unwrap_or(false)
}

/// Extract function/chunk names from a dispatch result JSON value.
///
/// Walks known array fields (results, chunks, callers, calls, tests, dead,
/// possibly_dead_pub, path, shared_callers, shared_callees, shared_types)
/// plus the top-level "name" field (explain). Deduplicates preserving order.
fn extract_names(val: &serde_json::Value) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut names = Vec::new();

    let mut push_name = |n: &str| {
        if !n.is_empty() && seen.insert(n.to_string()) {
            names.push(n.to_string());
        }
    };

    // Top-level "name" (explain returns the target's own name)
    if let Some(name) = val.get("name").and_then(|v| v.as_str()) {
        push_name(name);
    }

    // Bare array (callers returns [...])
    if let Some(arr) = val.as_array() {
        for item in arr {
            if let Some(name) = item.get("name").and_then(|v| v.as_str()) {
                push_name(name);
            }
        }
        return names;
    }

    // Known array fields in dispatch results
    const NAME_ARRAY_FIELDS: &[&str] = &[
        "results",           // search, similar
        "chunks",            // gather, context
        "callers",           // impact, explain
        "calls",             // callees
        "tests",             // impact, test-map
        "dead",              // dead
        "possibly_dead_pub", // dead
        "path",              // trace
        "shared_callers",    // related
        "shared_callees",    // related
        "shared_types",      // related
        "similar",           // explain
        "callees",           // explain
    ];

    for field in NAME_ARRAY_FIELDS {
        if let Some(arr) = val.get(*field).and_then(|v| v.as_array()) {
            for item in arr {
                if let Some(name) = item.get("name").and_then(|v| v.as_str()) {
                    push_name(name);
                }
            }
        }
    }

    // Scout: nested file_groups[].chunks[].name
    if let Some(groups) = val.get("file_groups").and_then(|v| v.as_array()) {
        for group in groups {
            if let Some(chunks) = group.get("chunks").and_then(|v| v.as_array()) {
                for chunk in chunks {
                    if let Some(name) = chunk.get("name").and_then(|v| v.as_str()) {
                        push_name(name);
                    }
                }
            }
        }
    }

    names
}

/// Split a token list by standalone `|` into pipeline segments.
fn split_tokens_by_pipe(tokens: &[String]) -> Vec<Vec<String>> {
    let mut segments: Vec<Vec<String>> = Vec::new();
    let mut current: Vec<String> = Vec::new();

    for token in tokens {
        if token == "|" {
            segments.push(std::mem::take(&mut current));
        } else {
            current.push(token.clone());
        }
    }
    segments.push(current);
    segments
}

/// Execute a pipeline: chain commands where upstream names feed downstream.
fn execute_pipeline(ctx: &BatchContext, tokens: &[String], raw_line: &str) -> serde_json::Value {
    let _span = tracing::info_span!("pipeline", input = raw_line).entered();

    let segments = split_tokens_by_pipe(tokens);
    let stage_count = segments.len();

    // Validate: no empty segments
    for (i, seg) in segments.iter().enumerate() {
        if seg.is_empty() {
            return serde_json::json!({"error": format!(
                "Empty pipeline segment at position {}", i + 1
            )});
        }
    }

    // Validate: downstream segments (index 1+) must be pipeable
    for seg in &segments[1..] {
        if !is_pipeable_command(seg) {
            let cmd = seg.first().map(|s| s.as_str()).unwrap_or("(empty)");
            return serde_json::json!({"error": format!(
                "Cannot pipe into '{}' \u{2014} it doesn't accept a function name. \
                 Pipeable commands: {}",
                cmd,
                PIPEABLE_COMMANDS.join(", ")
            )});
        }
    }

    // Validate: quit/exit/help not allowed in pipelines
    for seg in &segments {
        if let Some(first) = seg.first() {
            let lower = first.to_ascii_lowercase();
            if lower == "quit" || lower == "exit" || lower == "help" {
                return serde_json::json!({"error": format!(
                    "'{}' cannot be used in a pipeline", first
                )});
            }
        }
    }

    // Stage 0: execute first segment normally
    let stage0_result = {
        let _stage_span = tracing::info_span!(
            "pipeline_stage",
            stage = 0,
            command = segments[0].first().map(|s| s.as_str()).unwrap_or("?"),
        )
        .entered();

        match BatchInput::try_parse_from(&segments[0]) {
            Ok(input) => match dispatch(ctx, input.cmd) {
                Ok(val) => val,
                Err(e) => {
                    return serde_json::json!({"error": format!(
                        "Pipeline stage 1 failed: {}", e
                    )});
                }
            },
            Err(e) => {
                return serde_json::json!({"error": format!(
                    "Pipeline stage 1 parse error: {}", e
                )});
            }
        }
    };

    // Process remaining stages
    let mut current_value = stage0_result;
    let mut any_truncated = false;

    for (stage_idx, segment) in segments[1..].iter().enumerate() {
        let stage_num = stage_idx + 1; // 1-indexed for display (stage 0 already done)

        // Extract names from current result
        let mut names = extract_names(&current_value);
        tracing::debug!(stage = stage_num, count = names.len(), "Names extracted");

        if names.len() > PIPELINE_FAN_OUT_LIMIT {
            any_truncated = true;
            tracing::info!(
                stage = stage_num,
                original = names.len(),
                limit = PIPELINE_FAN_OUT_LIMIT,
                "Fan-out truncated"
            );
            names.truncate(PIPELINE_FAN_OUT_LIMIT);
        }

        let total_inputs = names.len();
        let _stage_span = tracing::info_span!(
            "pipeline_stage",
            stage = stage_num + 1, // 1-based for user
            command = segment.first().map(|s| s.as_str()).unwrap_or("?"),
            fan_out = total_inputs,
        )
        .entered();

        if names.is_empty() {
            // No names to fan out — return empty pipeline result
            return build_pipeline_result(raw_line, stage_count, vec![], vec![], 0, false);
        }

        let mut results: Vec<(String, serde_json::Value)> = Vec::new();
        let mut errors: Vec<(String, String)> = Vec::new();

        for name in &names {
            // Build tokens: prepend name to downstream segment
            let mut cmd_tokens = vec![segment[0].clone(), name.clone()];
            cmd_tokens.extend_from_slice(&segment[1..]);

            match BatchInput::try_parse_from(&cmd_tokens) {
                Ok(input) => match dispatch(ctx, input.cmd) {
                    Ok(val) => results.push((name.clone(), val)),
                    Err(e) => {
                        tracing::warn!(name = name, error = %e, "Per-name dispatch failed");
                        errors.push((name.clone(), e.to_string()));
                    }
                },
                Err(e) => {
                    tracing::warn!(name = name, error = %e, "Per-name parse failed");
                    errors.push((name.clone(), e.to_string()));
                }
            }
        }

        // If this is the last stage, build the pipeline result envelope
        if stage_num == segments.len() - 1 {
            return build_pipeline_result(
                raw_line,
                stage_count,
                results,
                errors,
                total_inputs,
                any_truncated,
            );
        }

        // Intermediate stage: merge results for next stage's name extraction
        // Collect all per-name results into a single object with all names
        let mut merged_names: Vec<String> = Vec::new();
        let mut merged_seen = HashSet::new();
        for (_, val) in &results {
            for n in extract_names(val) {
                if merged_seen.insert(n.clone()) {
                    merged_names.push(n);
                }
            }
        }

        // Build a synthetic value with a "results" array for extraction
        let synthetic: Vec<serde_json::Value> = merged_names
            .iter()
            .map(|n| serde_json::json!({"name": n}))
            .collect();
        current_value = serde_json::json!({"results": synthetic});
    }

    // Should not reach here, but safety net
    serde_json::json!({"error": "Pipeline execution ended unexpectedly"})
}

/// Build the final pipeline result envelope.
fn build_pipeline_result(
    pipeline_str: &str,
    stages: usize,
    results: Vec<(String, serde_json::Value)>,
    errors: Vec<(String, String)>,
    total_inputs: usize,
    truncated: bool,
) -> serde_json::Value {
    let results_json: Vec<serde_json::Value> = results
        .into_iter()
        .map(|(input, data)| serde_json::json!({"_input": input, "data": data}))
        .collect();

    let errors_json: Vec<serde_json::Value> = errors
        .into_iter()
        .map(|(input, err)| serde_json::json!({"_input": input, "error": err}))
        .collect();

    serde_json::json!({
        "pipeline": pipeline_str,
        "stages": stages,
        "results": results_json,
        "errors": errors_json,
        "total_inputs": total_inputs,
        "truncated": truncated,
    })
}

/// Check if a token list contains a pipeline (standalone `|` token).
fn has_pipe_token(tokens: &[String]) -> bool {
    tokens.iter().any(|t| t == "|")
}

// ─── Main loop ───────────────────────────────────────────────────────────────

/// Entry point for `cqs batch`.
pub(crate) fn cmd_batch(_cli: &super::Cli) -> Result<()> {
    let _span = tracing::info_span!("cmd_batch").entered();

    let (store, root, cqs_dir) = open_project_store()?;
    let ctx = BatchContext {
        store,
        embedder: OnceLock::new(),
        hnsw: OnceLock::new(),
        refs: RefCell::new(HashMap::new()),
        root,
        cqs_dir,
        file_set: OnceLock::new(),
        audit_state: OnceLock::new(),
        notes_cache: OnceLock::new(),
    };

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to read stdin line");
                break;
            }
        };

        let trimmed = line.trim();

        // Skip empty lines and comments
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        // Quit/exit
        if trimmed.eq_ignore_ascii_case("quit") || trimmed.eq_ignore_ascii_case("exit") {
            break;
        }

        // Tokenize the line
        let tokens = match shell_words::split(trimmed) {
            Ok(t) => t,
            Err(e) => {
                let error_json = serde_json::json!({"error": format!("Parse error: {}", e)});
                let _ = writeln!(stdout, "{}", serde_json::to_string(&error_json).unwrap());
                let _ = stdout.flush();
                continue;
            }
        };

        if tokens.is_empty() {
            continue;
        }

        // Pipeline detection: if tokens contain a standalone `|`, route to pipeline
        if has_pipe_token(&tokens) {
            let result = execute_pipeline(&ctx, &tokens, trimmed);
            let _ = writeln!(stdout, "{}", serde_json::to_string(&result).unwrap());
        } else {
            // Single command — existing path
            match BatchInput::try_parse_from(&tokens) {
                Ok(input) => match dispatch(&ctx, input.cmd) {
                    Ok(value) => {
                        let _ = writeln!(stdout, "{}", serde_json::to_string(&value).unwrap());
                    }
                    Err(e) => {
                        let error_json = serde_json::json!({"error": format!("{}", e)});
                        let _ = writeln!(stdout, "{}", serde_json::to_string(&error_json).unwrap());
                    }
                },
                Err(e) => {
                    let error_json = serde_json::json!({"error": format!("{}", e)});
                    let _ = writeln!(stdout, "{}", serde_json::to_string(&error_json).unwrap());
                }
            }
        }

        let _ = stdout.flush();
    }

    Ok(())
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn test_parse_search() {
        let input = BatchInput::try_parse_from(["search", "hello"]).unwrap();
        match input.cmd {
            BatchCmd::Search {
                ref query, limit, ..
            } => {
                assert_eq!(query, "hello");
                assert_eq!(limit, 5); // default
            }
            _ => panic!("Expected Search command"),
        }
    }

    #[test]
    fn test_parse_search_with_flags() {
        let input =
            BatchInput::try_parse_from(["search", "hello", "--limit", "3", "--name-only"]).unwrap();
        match input.cmd {
            BatchCmd::Search {
                ref query,
                limit,
                name_only,
                ..
            } => {
                assert_eq!(query, "hello");
                assert_eq!(limit, 3);
                assert!(name_only);
            }
            _ => panic!("Expected Search command"),
        }
    }

    #[test]
    fn test_parse_callers() {
        let input = BatchInput::try_parse_from(["callers", "my_func"]).unwrap();
        match input.cmd {
            BatchCmd::Callers { ref name } => assert_eq!(name, "my_func"),
            _ => panic!("Expected Callers command"),
        }
    }

    #[test]
    fn test_parse_gather_with_ref() {
        let input =
            BatchInput::try_parse_from(["gather", "alarm config", "--ref", "aveva"]).unwrap();
        match input.cmd {
            BatchCmd::Gather {
                ref query,
                ref ref_name,
                ..
            } => {
                assert_eq!(query, "alarm config");
                assert_eq!(ref_name.as_deref(), Some("aveva"));
            }
            _ => panic!("Expected Gather command"),
        }
    }

    #[test]
    fn test_parse_dead_with_confidence() {
        let input =
            BatchInput::try_parse_from(["dead", "--min-confidence", "high", "--include-pub"])
                .unwrap();
        match input.cmd {
            BatchCmd::Dead {
                include_pub,
                ref min_confidence,
            } => {
                assert!(include_pub);
                assert!(matches!(min_confidence, DeadConfidenceLevel::High));
            }
            _ => panic!("Expected Dead command"),
        }
    }

    #[test]
    fn test_parse_unknown_command() {
        let result = BatchInput::try_parse_from(["bogus"]);
        assert!(result.is_err());
    }

    // ─── Pipeline unit tests ─────────────────────────────────────────────

    #[test]
    fn test_extract_names_search_result() {
        let val = serde_json::json!({
            "results": [{"name": "a", "file": "f.rs"}, {"name": "b", "file": "g.rs"}],
            "query": "test",
            "total": 2
        });
        assert_eq!(extract_names(&val), vec!["a", "b"]);
    }

    #[test]
    fn test_extract_names_callers_bare_array() {
        let val = serde_json::json!([{"name": "a", "file": "f.rs"}, {"name": "b", "file": "g.rs"}]);
        assert_eq!(extract_names(&val), vec!["a", "b"]);
    }

    #[test]
    fn test_extract_names_callees() {
        let val = serde_json::json!({
            "function": "f",
            "calls": [{"name": "a", "line": 1}],
            "count": 1
        });
        assert_eq!(extract_names(&val), vec!["a"]);
    }

    #[test]
    fn test_extract_names_impact() {
        let val = serde_json::json!({
            "function": "f",
            "callers": [{"name": "a"}],
            "tests": [{"name": "b"}],
            "caller_count": 1,
            "test_count": 1
        });
        let names = extract_names(&val);
        assert!(names.contains(&"a".to_string()));
        assert!(names.contains(&"b".to_string()));
    }

    #[test]
    fn test_extract_names_dead() {
        let val = serde_json::json!({
            "dead": [{"name": "a"}],
            "possibly_dead_pub": [{"name": "b"}],
            "total_dead": 1,
            "total_possibly_dead_pub": 1
        });
        let names = extract_names(&val);
        assert!(names.contains(&"a".to_string()));
        assert!(names.contains(&"b".to_string()));
    }

    #[test]
    fn test_extract_names_related() {
        let val = serde_json::json!({
            "target": "f",
            "shared_callers": [{"name": "a"}],
            "shared_callees": [{"name": "b"}],
            "shared_types": [{"name": "c"}]
        });
        let names = extract_names(&val);
        assert!(names.contains(&"a".to_string()));
        assert!(names.contains(&"b".to_string()));
        assert!(names.contains(&"c".to_string()));
    }

    #[test]
    fn test_extract_names_trace() {
        let val = serde_json::json!({
            "source": "s",
            "target": "t",
            "path": [{"name": "a"}, {"name": "b"}],
            "depth": 1
        });
        let names = extract_names(&val);
        assert!(names.contains(&"a".to_string()));
        assert!(names.contains(&"b".to_string()));
    }

    #[test]
    fn test_extract_names_explain() {
        let val = serde_json::json!({
            "name": "target",
            "callers": [{"name": "a"}],
            "similar": [{"name": "b"}]
        });
        let names = extract_names(&val);
        assert_eq!(names[0], "target"); // top-level name first
        assert!(names.contains(&"a".to_string()));
        assert!(names.contains(&"b".to_string()));
    }

    #[test]
    fn test_extract_names_empty_results() {
        let val = serde_json::json!({"results": [], "query": "x", "total": 0});
        assert!(extract_names(&val).is_empty());
    }

    #[test]
    fn test_extract_names_stats_no_names() {
        let val = serde_json::json!({
            "total_chunks": 100,
            "total_files": 10,
            "notes": 5
        });
        assert!(extract_names(&val).is_empty());
    }

    #[test]
    fn test_extract_names_dedup() {
        let val = serde_json::json!({
            "results": [{"name": "a"}, {"name": "a"}, {"name": "b"}]
        });
        assert_eq!(extract_names(&val), vec!["a", "b"]);
    }

    #[test]
    fn test_is_pipeable_callers() {
        assert!(is_pipeable_command(&["callers".to_string()]));
    }

    #[test]
    fn test_is_pipeable_search() {
        assert!(!is_pipeable_command(&[
            "search".to_string(),
            "foo".to_string()
        ]));
    }

    #[test]
    fn test_is_pipeable_stats() {
        assert!(!is_pipeable_command(&["stats".to_string()]));
    }

    #[test]
    fn test_split_tokens_by_pipe() {
        let tokens: Vec<String> = vec!["search", "foo", "|", "callers"]
            .into_iter()
            .map(String::from)
            .collect();
        let segments = split_tokens_by_pipe(&tokens);
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0], vec!["search", "foo"]);
        assert_eq!(segments[1], vec!["callers"]);
    }

    #[test]
    fn test_split_tokens_three_stages() {
        let tokens: Vec<String> = vec!["search", "foo", "|", "callers", "|", "test-map"]
            .into_iter()
            .map(String::from)
            .collect();
        let segments = split_tokens_by_pipe(&tokens);
        assert_eq!(segments.len(), 3);
        assert_eq!(segments[0], vec!["search", "foo"]);
        assert_eq!(segments[1], vec!["callers"]);
        assert_eq!(segments[2], vec!["test-map"]);
    }

    #[test]
    fn test_has_pipe_token() {
        let with_pipe: Vec<String> = vec!["search", "foo", "|", "callers"]
            .into_iter()
            .map(String::from)
            .collect();
        assert!(has_pipe_token(&with_pipe));

        let without_pipe: Vec<String> = vec!["search", "foo|bar"]
            .into_iter()
            .map(String::from)
            .collect();
        assert!(!has_pipe_token(&without_pipe));
    }

    #[test]
    fn test_parse_trace() {
        let input = BatchInput::try_parse_from(["trace", "main", "validate"]).unwrap();
        match input.cmd {
            BatchCmd::Trace {
                ref source,
                ref target,
                max_depth,
            } => {
                assert_eq!(source, "main");
                assert_eq!(target, "validate");
                assert_eq!(max_depth, 10); // default
            }
            _ => panic!("Expected Trace command"),
        }
    }

    #[test]
    fn test_parse_context() {
        let input = BatchInput::try_parse_from(["context", "src/lib.rs", "--compact"]).unwrap();
        match input.cmd {
            BatchCmd::Context {
                ref path,
                compact,
                summary,
                ..
            } => {
                assert_eq!(path, "src/lib.rs");
                assert!(compact);
                assert!(!summary);
            }
            _ => panic!("Expected Context command"),
        }
    }

    #[test]
    fn test_parse_stats() {
        let input = BatchInput::try_parse_from(["stats"]).unwrap();
        assert!(matches!(input.cmd, BatchCmd::Stats));
    }

    #[test]
    fn test_parse_impact_with_suggest() {
        let input =
            BatchInput::try_parse_from(["impact", "foo", "--depth", "3", "--suggest-tests"])
                .unwrap();
        match input.cmd {
            BatchCmd::Impact {
                ref name,
                depth,
                suggest_tests,
                include_types,
            } => {
                assert_eq!(name, "foo");
                assert_eq!(depth, 3);
                assert!(suggest_tests);
                assert!(!include_types);
            }
            _ => panic!("Expected Impact command"),
        }
    }

    // ─── New command parse tests ──────────────────────────────────────────

    #[test]
    fn test_parse_scout() {
        let input = BatchInput::try_parse_from(["scout", "error handling"]).unwrap();
        match input.cmd {
            BatchCmd::Scout {
                ref query, limit, ..
            } => {
                assert_eq!(query, "error handling");
                assert_eq!(limit, 10); // default
            }
            _ => panic!("Expected Scout command"),
        }
    }

    #[test]
    fn test_parse_scout_with_flags() {
        let input = BatchInput::try_parse_from([
            "scout",
            "error handling",
            "--limit",
            "20",
            "--tokens",
            "2000",
        ])
        .unwrap();
        match input.cmd {
            BatchCmd::Scout {
                ref query,
                limit,
                tokens,
            } => {
                assert_eq!(query, "error handling");
                assert_eq!(limit, 20);
                assert_eq!(tokens, Some(2000));
            }
            _ => panic!("Expected Scout command"),
        }
    }

    #[test]
    fn test_parse_where() {
        let input = BatchInput::try_parse_from(["where", "new CLI command"]).unwrap();
        match input.cmd {
            BatchCmd::Where {
                ref description,
                limit,
            } => {
                assert_eq!(description, "new CLI command");
                assert_eq!(limit, 5); // default
            }
            _ => panic!("Expected Where command"),
        }
    }

    #[test]
    fn test_parse_read() {
        let input = BatchInput::try_parse_from(["read", "src/lib.rs"]).unwrap();
        match input.cmd {
            BatchCmd::Read {
                ref path,
                ref focus,
            } => {
                assert_eq!(path, "src/lib.rs");
                assert!(focus.is_none());
            }
            _ => panic!("Expected Read command"),
        }
    }

    #[test]
    fn test_parse_read_focused() {
        let input =
            BatchInput::try_parse_from(["read", "src/lib.rs", "--focus", "enumerate_files"])
                .unwrap();
        match input.cmd {
            BatchCmd::Read {
                ref path,
                ref focus,
            } => {
                assert_eq!(path, "src/lib.rs");
                assert_eq!(focus.as_deref(), Some("enumerate_files"));
            }
            _ => panic!("Expected Read command"),
        }
    }

    #[test]
    fn test_parse_stale() {
        let input = BatchInput::try_parse_from(["stale"]).unwrap();
        assert!(matches!(input.cmd, BatchCmd::Stale));
    }

    #[test]
    fn test_parse_health() {
        let input = BatchInput::try_parse_from(["health"]).unwrap();
        assert!(matches!(input.cmd, BatchCmd::Health));
    }

    #[test]
    fn test_parse_notes() {
        let input = BatchInput::try_parse_from(["notes"]).unwrap();
        match input.cmd {
            BatchCmd::Notes { warnings, patterns } => {
                assert!(!warnings);
                assert!(!patterns);
            }
            _ => panic!("Expected Notes command"),
        }
    }

    #[test]
    fn test_parse_notes_warnings() {
        let input = BatchInput::try_parse_from(["notes", "--warnings"]).unwrap();
        match input.cmd {
            BatchCmd::Notes { warnings, patterns } => {
                assert!(warnings);
                assert!(!patterns);
            }
            _ => panic!("Expected Notes command"),
        }
    }

    #[test]
    fn test_parse_notes_patterns() {
        let input = BatchInput::try_parse_from(["notes", "--patterns"]).unwrap();
        match input.cmd {
            BatchCmd::Notes { warnings, patterns } => {
                assert!(!warnings);
                assert!(patterns);
            }
            _ => panic!("Expected Notes command"),
        }
    }

    #[test]
    fn test_extract_names_scout() {
        let val = serde_json::json!({
            "file_groups": [
                {
                    "file": "src/search.rs",
                    "chunks": [
                        {"name": "search_filtered", "role": "modify_target"},
                        {"name": "resolve_target", "role": "dependency"}
                    ]
                },
                {
                    "file": "src/store.rs",
                    "chunks": [
                        {"name": "open_store", "role": "modify_target"}
                    ]
                }
            ],
            "summary": {"total_files": 2}
        });
        let names = extract_names(&val);
        assert_eq!(names.len(), 3);
        assert!(names.contains(&"search_filtered".to_string()));
        assert!(names.contains(&"resolve_target".to_string()));
        assert!(names.contains(&"open_store".to_string()));
    }

    #[test]
    fn test_is_pipeable_scout() {
        assert!(is_pipeable_command(&[
            "scout".to_string(),
            "foo".to_string()
        ]));
    }
}
