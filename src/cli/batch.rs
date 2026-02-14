//! Batch mode — persistent Store + Embedder, JSONL output
//!
//! Reads commands from stdin, executes against a shared Store and lazily-loaded
//! Embedder, outputs compact JSON per line. Amortizes ~100ms Store open and
//! ~500ms Embedder ONNX init across N commands.
//!
//! This is step 2 of the `cqs chat` build path. Step 3 (REPL) wraps
//! `BatchContext` + `dispatch()` with readline.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::OnceLock;

use anyhow::Result;
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
        } => dispatch_impact(ctx, &name, depth, suggest_tests),
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
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_impact", name).entered();

    let resolved = cqs::resolve_target(&ctx.store, name)?;
    let chunk = &resolved.chunk;
    let depth = depth.clamp(1, 10);

    let result = cqs::analyze_impact(&ctx.store, &chunk.name, depth)?;

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

    Ok(serde_json::json!({
        "total_chunks": stats.total_chunks,
        "total_files": stats.total_files,
        "notes": note_count,
        "call_graph": {
            "total_calls": fc_stats.total_calls,
            "unique_callers": fc_stats.unique_callers,
            "unique_callees": fc_stats.unique_callees,
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

fn dispatch_help() -> Result<serde_json::Value> {
    use clap::CommandFactory;
    let mut buf = Vec::new();
    BatchInput::command().write_help(&mut buf)?;
    let help_text = String::from_utf8_lossy(&buf).to_string();
    Ok(serde_json::json!({"help": help_text}))
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

        // Parse and dispatch
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
            } => {
                assert_eq!(name, "foo");
                assert_eq!(depth, 3);
                assert!(suggest_tests);
            }
            _ => panic!("Expected Impact command"),
        }
    }
}
