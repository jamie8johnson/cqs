//! Smart context assembly — given a question, return the minimal code set to answer it.
//!
//! Algorithm:
//! 1. Search for seed results
//! 2. BFS expand via call graph (callers/callees/both)
//! 3. Cap expansion at 200 nodes
//! 4. Deduplicate by parent_id
//! 5. Sort by file → line (reading order)

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rayon::prelude::*;

use crate::parser::{ChunkType, Language};
use crate::{AnalysisError, Embedder, Embedding};

use crate::store::helpers::{CallGraph, SearchFilter};
use crate::store::SearchResult;
use crate::Store;

/// Default maximum nodes in BFS expansion to prevent blowup on hub functions.
pub const DEFAULT_MAX_EXPANDED_NODES: usize = 200;

/// Options for gather operation
#[derive(Debug, Clone)]
pub struct GatherOptions {
    pub expand_depth: usize,
    pub direction: GatherDirection,
    pub limit: usize,
    pub seed_limit: usize,
    pub seed_threshold: f32,
    pub decay_factor: f32,
    /// Maximum nodes in BFS expansion (default: 200).
    /// Prevents blowup on hub functions with many callers/callees.
    pub max_expanded_nodes: usize,
    /// Pre-computed query embedding. When set, `gather()` uses this instead
    /// of calling `embedder.embed_query()`. Useful when the caller shares one
    /// embedding across multiple phases (e.g., `task()` reuses it for scout + gather).
    pub query_embedding: Option<Embedding>,
}

impl GatherOptions {
    /// Sets the maximum depth for expanding nested structures in output.
    ///
    /// # Arguments
    ///
    /// * `depth` - The maximum nesting level to expand before truncating or summarizing nested content.
    ///
    /// # Returns
    ///
    /// Returns `self` to allow method chaining.
    pub fn with_expand_depth(mut self, depth: usize) -> Self {
        self.expand_depth = depth;
        self
    }
    /// Sets the direction for gathering operations.
    ///
    /// # Arguments
    ///
    /// * `direction` - The direction to use when gathering data
    ///
    /// # Returns
    ///
    /// Returns `self` to allow method chaining.
    pub fn with_direction(mut self, direction: GatherDirection) -> Self {
        self.direction = direction;
        self
    }
    /// Sets the maximum number of items to process.
    ///
    /// # Arguments
    ///
    /// * `limit` - The maximum number of items to process
    ///
    /// # Returns
    ///
    /// Returns `self` to allow method chaining.
    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }
    /// Sets the maximum number of seeds to process.
    ///
    /// # Arguments
    ///
    /// * `limit` - The maximum number of seeds to process
    ///
    /// # Returns
    ///
    /// Returns `self` to allow method chaining.
    pub fn with_seed_limit(mut self, limit: usize) -> Self {
        self.seed_limit = limit;
        self
    }
    /// Sets the seed threshold for this builder and returns self for method chaining.
    ///
    /// # Arguments
    ///
    /// * `threshold` - The seed threshold value (typically between 0.0 and 1.0) used to filter seeds during processing
    ///
    /// # Returns
    ///
    /// Returns `Self` to allow for method chaining in builder pattern style construction.
    pub fn with_seed_threshold(mut self, threshold: f32) -> Self {
        // RT-RES-7: Guard against NaN — use default (0.3) if threshold is not finite.
        if threshold.is_finite() {
            self.seed_threshold = threshold;
        } else {
            tracing::warn!(
                threshold = %threshold,
                "NaN/infinite seed threshold, using default 0.3"
            );
            self.seed_threshold = 0.3;
        }
        self
    }
    /// Sets the decay factor for this builder, clamping the value to the range [0.0, 1.0].
    ///
    /// If the provided factor is not a finite number (NaN or infinite), the current decay factor is preserved unchanged.
    ///
    /// # Arguments
    ///
    /// * `factor` - The decay factor to set, typically a value between 0.0 and 1.0
    ///
    /// # Returns
    ///
    /// Returns `self` to allow for method chaining.
    pub fn with_decay_factor(mut self, factor: f32) -> Self {
        self.decay_factor = if factor.is_finite() {
            factor.clamp(0.0, 1.0)
        } else {
            self.decay_factor
        };
        self
    }
    /// Sets the maximum number of nodes that can be expanded during search.
    ///
    /// # Arguments
    ///
    /// * `max` - The maximum number of nodes to expand. When the search expands more nodes than this limit, it will stop exploring further branches.
    ///
    /// # Returns
    ///
    /// Returns `self` to allow method chaining.
    pub fn with_max_expanded_nodes(mut self, max: usize) -> Self {
        self.max_expanded_nodes = max;
        self
    }
}

/// PF-10: Read CQS_GATHER_MAX_NODES once via OnceLock, not on every GatherOptions::default().
fn gather_max_nodes() -> usize {
    static CAP: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *CAP.get_or_init(|| match std::env::var("CQS_GATHER_MAX_NODES") {
        Ok(val) => match val.parse::<usize>() {
            Ok(n) if n > 0 => {
                tracing::info!(
                    max_nodes = n,
                    "BFS node cap overridden via CQS_GATHER_MAX_NODES"
                );
                n
            }
            _ => {
                tracing::warn!(
                    value = %val,
                    "Invalid CQS_GATHER_MAX_NODES, using default {}",
                    DEFAULT_MAX_EXPANDED_NODES
                );
                DEFAULT_MAX_EXPANDED_NODES
            }
        },
        Err(_) => DEFAULT_MAX_EXPANDED_NODES,
    })
}

impl Default for GatherOptions {
    /// Creates a new instance with default configuration values for graph traversal and expansion.
    ///
    /// The default settings prioritize balanced exploration with a depth of 1, bidirectional search, a limit of 10 results, and a decay factor of 0.8 to gradually reduce node relevance as distance increases.
    ///
    /// # Returns
    ///
    /// A new `Self` instance initialized with sensible defaults for graph expansion operations.
    fn default() -> Self {
        Self {
            expand_depth: 1,
            direction: GatherDirection::Both,
            limit: 10,
            seed_limit: 5,
            seed_threshold: 0.3,
            decay_factor: 0.8,
            max_expanded_nodes: gather_max_nodes(),
            query_embedding: None,
        }
    }
}

/// Direction of call graph expansion
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, clap::ValueEnum)]
pub enum GatherDirection {
    Both,
    Callers,
    Callees,
}

impl std::str::FromStr for GatherDirection {
    type Err = String;
    /// Parses a string into a direction value.
    ///
    /// Converts a string representation into the corresponding direction variant. Valid inputs are "both", "callers", or "callees" (case-sensitive).
    ///
    /// # Arguments
    ///
    /// * `s` - A string slice to parse as a direction
    ///
    /// # Returns
    ///
    /// Returns `Ok(Self)` with the parsed direction variant if the input is valid, or `Err(String)` with a descriptive error message if the input is not recognized.
    fn from_str(s: &str) -> std::result::Result<Self, String> {
        match s {
            "both" => Ok(Self::Both),
            "callers" => Ok(Self::Callers),
            "callees" => Ok(Self::Callees),
            _ => Err(format!(
                "Invalid direction '{}'. Valid: both, callers, callees",
                s
            )),
        }
    }
}

/// A gathered code chunk with context
#[derive(Debug, Clone, serde::Serialize)]
pub struct GatheredChunk {
    pub name: String,
    #[serde(serialize_with = "crate::serialize_path_normalized")]
    pub file: PathBuf,
    pub line_start: u32,
    pub line_end: u32,
    pub language: Language,
    pub chunk_type: ChunkType,
    pub signature: String,
    pub content: String,
    pub score: f32,
    pub depth: usize,
    /// Source: None = project, Some(name) = reference
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

impl GatheredChunk {
    /// Build from a `SearchResult`, relativizing its file path against `root`.
    pub fn from_search(
        sr: &crate::store::SearchResult,
        root: &Path,
        score: f32,
        depth: usize,
        source: Option<String>,
    ) -> Self {
        Self {
            name: sr.chunk.name.clone(),
            file: sr
                .chunk
                .file
                .strip_prefix(root)
                .unwrap_or(&sr.chunk.file)
                .to_path_buf(),
            line_start: sr.chunk.line_start,
            line_end: sr.chunk.line_end,
            language: sr.chunk.language,
            chunk_type: sr.chunk.chunk_type,
            signature: sr.chunk.signature.clone(),
            content: sr.chunk.content.clone(),
            score,
            depth,
            source,
        }
    }
}

/// Result of a gather operation
#[derive(Debug, Clone, serde::Serialize)]
pub struct GatherResult {
    pub chunks: Vec<GatheredChunk>,
    pub expansion_capped: bool,
    /// True if batch name search failed and results may be incomplete
    pub search_degraded: bool,
}

// MAX_EXPANDED_NODES is now configurable via GatherOptions::max_expanded_nodes
// (default: DEFAULT_MAX_EXPANDED_NODES = 200)

/// BFS-expand names via call graph, applying score decay and enforcing a node cap.
///
/// Returns `(name_scores, expansion_capped)` where `name_scores` maps
/// function names to `(score, depth)`.
pub(crate) fn bfs_expand(
    name_scores: &mut HashMap<String, (f32, usize)>,
    graph: &CallGraph,
    opts: &GatherOptions,
) -> bool {
    let mut expansion_capped = false;
    if opts.expand_depth == 0 {
        return false;
    }

    // Track visited nodes to prevent re-expansion from overlapping seeds.
    // Without this, if two seeds overlap (e.g., bridge results matching ref seeds
    // in gather_cross_index), the same node gets expanded twice, doubling neighbor lookups.
    // PF-2: Use Arc<str> in visited/queue to avoid per-node String heap allocations.
    let mut visited: HashSet<Arc<str>> =
        name_scores.keys().map(|k| Arc::from(k.as_str())).collect();
    let initial_size = name_scores.len();

    let mut queue: VecDeque<(Arc<str>, usize)> = VecDeque::new();
    for name in name_scores.keys() {
        queue.push_back((Arc::from(name.as_str()), 0));
    }

    while let Some((name, depth)) = queue.pop_front() {
        if depth >= opts.expand_depth {
            continue;
        }
        if name_scores.len() >= opts.max_expanded_nodes && visited.len() > initial_size {
            expansion_capped = true;
            break;
        }

        let neighbors = get_neighbors(graph, &name, opts.direction);
        let base_score = name_scores
            .get(name.as_ref())
            .map(|(s, _)| *s)
            .unwrap_or(0.5);
        let new_score = base_score * opts.decay_factor;
        for neighbor in neighbors {
            if name_scores.len() >= opts.max_expanded_nodes {
                expansion_capped = true;
                break;
            }
            if !visited.contains(&neighbor) {
                visited.insert(Arc::clone(&neighbor));
                let key: String = neighbor.to_string();
                name_scores.insert(key, (new_score, depth + 1));
                queue.push_back((neighbor, depth + 1));
            } else if let Some(existing) = name_scores.get_mut(neighbor.as_ref()) {
                // Already visited — update score if higher, preserve minimum depth.
                // AC-13: We intentionally do NOT re-enqueue the node. Re-enqueueing
                // would propagate the higher score to neighbors (Dijkstra-like), but
                // risks exponential blowup on dense graphs. The max_expanded_nodes cap
                // keeps memory bounded, and the score update here ensures the node itself
                // gets the best available score even without re-expansion.
                if new_score > existing.0 {
                    existing.0 = new_score;
                    existing.1 = existing.1.min(depth + 1);
                }
            }
        }
        if expansion_capped {
            break;
        }
    }
    expansion_capped
}

/// Batch-fetch chunks for expanded names, deduplicate by id, assemble `GatheredChunk`s.
///
/// Returns `(chunks, search_degraded)`.
pub(crate) fn fetch_and_assemble(
    store: &Store,
    name_scores: &HashMap<String, (f32, usize)>,
    root: &Path,
) -> (Vec<GatheredChunk>, bool) {
    let all_names: Vec<&str> = name_scores.keys().map(|s| s.as_str()).collect();
    let (batch_results, search_degraded) = match store.search_by_names_batch(&all_names, 1) {
        Ok(r) => (r, false),
        Err(e) => {
            tracing::warn!(error = %e, "Batch name search failed, results may be incomplete");
            (HashMap::new(), true)
        }
    };

    let mut seen_ids: HashSet<String> = HashSet::new();
    let mut chunks: Vec<GatheredChunk> = Vec::new();

    for (name, (score, depth)) in name_scores {
        if let Some(results) = batch_results.get(name) {
            if let Some(r) = results.first() {
                if seen_ids.contains(&r.chunk.id) {
                    continue;
                }
                seen_ids.insert(r.chunk.id.clone());

                chunks.push(GatheredChunk::from_search(r, root, *score, *depth, None));
            }
        }
    }

    tracing::debug!(chunk_count = chunks.len(), "Chunks assembled");
    (chunks, search_degraded)
}

/// Sort chunks by score desc (file/line/name tiebreak), truncate to limit,
/// then re-sort to file/line reading order.
///
/// The tiebreak cascade (name → file → line_start) is full — two chunks can
/// collide on `name` when they're from different files (e.g., two `new`
/// functions), so we also compare `file` and `line_start` to keep the
/// truncate() boundary deterministic across process invocations.
pub(crate) fn sort_and_truncate(chunks: &mut Vec<GatheredChunk>, limit: usize) {
    chunks.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(a.name.cmp(&b.name))
            .then(a.file.cmp(&b.file))
            .then(a.line_start.cmp(&b.line_start))
    });
    chunks.truncate(limit);
    chunks.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then(a.line_start.cmp(&b.line_start))
            .then(a.name.cmp(&b.name))
    });
}

/// Gather relevant code chunks for a query.
///
/// Embeds the query internally (or uses `opts.query_embedding` if pre-computed).
/// Loads the call graph internally. For pre-loaded graph, use [`gather_with_graph`].
pub fn gather(
    store: &Store,
    embedder: &Embedder,
    description: &str,
    opts: &GatherOptions,
    root: &Path,
) -> Result<GatherResult, AnalysisError> {
    let query_embedding = match &opts.query_embedding {
        Some(emb) => emb.clone(),
        None => embedder.embed_query(description)?,
    };
    let graph = store.get_call_graph()?;
    gather_with_graph(store, &query_embedding, description, opts, root, &graph)
}

/// Like [`gather`] but accepts a pre-loaded call graph.
///
/// Use when the caller already has the graph (e.g., batch mode or `task()`
/// which shares the graph across phases).
pub fn gather_with_graph(
    store: &Store,
    query_embedding: &crate::Embedding,
    query_text: &str,
    opts: &GatherOptions,
    root: &Path,
    graph: &CallGraph,
) -> Result<GatherResult, AnalysisError> {
    let _span = tracing::info_span!(
        "gather",
        query_len = query_text.len(),
        expand_depth = opts.expand_depth,
        limit = opts.limit
    )
    .entered();

    // 1. Seed with hybrid RRF search (not raw embedding-only)
    let filter = SearchFilter {
        query_text: query_text.to_string(),
        enable_rrf: false, // RRF off by default — pure cosine is faster + higher R@1 on expanded eval
        ..SearchFilter::default()
    };
    let seed_results = store.search_filtered(
        query_embedding,
        &filter,
        opts.seed_limit,
        opts.seed_threshold,
    )?;
    tracing::debug!(seed_count = seed_results.len(), "Seed search complete");
    if seed_results.is_empty() {
        return Ok(GatherResult {
            chunks: Vec::new(),
            expansion_capped: false,
            search_degraded: false,
        });
    }

    // Seed names with their scores
    let mut name_scores: HashMap<String, (f32, usize)> = HashMap::new();
    for r in &seed_results {
        name_scores.insert(r.chunk.name.clone(), (r.score, 0));
    }

    // 2. BFS expand
    let expansion_capped = bfs_expand(&mut name_scores, graph, opts);
    tracing::info!(
        expanded = name_scores.len(),
        capped = expansion_capped,
        "BFS expansion complete"
    );

    // 3. Batch-fetch chunks, deduplicate
    let (mut chunks, search_degraded) = fetch_and_assemble(store, &name_scores, root);

    // 4. Sort by score desc, truncate to limit, re-sort to reading order
    sort_and_truncate(&mut chunks, opts.limit);

    tracing::info!(final_chunks = chunks.len(), "Gather complete");

    Ok(GatherResult {
        chunks,
        expansion_capped,
        search_degraded,
    })
}

/// Cross-index gather: seed from a reference index, bridge into project code, BFS expand.
///
/// Flow:
/// 1. Search reference index for seed chunks matching the query
/// 2. Retrieve seed chunk embeddings from the reference store
/// 3. For each seed embedding, search the project store for similar code (bridge)
/// 4. BFS expand project-side bridges via the project call graph
/// 5. Return both reference seeds (context) and expanded project chunks
pub fn gather_cross_index(
    project_store: &Store,
    ref_idx: &crate::reference::ReferenceIndex,
    query_embedding: &crate::Embedding,
    query_text: &str,
    opts: &GatherOptions,
    root: &Path,
) -> Result<GatherResult, AnalysisError> {
    gather_cross_index_with_index(
        project_store,
        ref_idx,
        query_embedding,
        query_text,
        opts,
        root,
        None,
    )
}

/// Like [`gather_cross_index`] but accepts an optional HNSW index for O(log n)
/// bridge searches instead of brute-force scans per reference seed.
pub fn gather_cross_index_with_index(
    project_store: &Store,
    ref_idx: &crate::reference::ReferenceIndex,
    query_embedding: &crate::Embedding,
    query_text: &str,
    opts: &GatherOptions,
    root: &Path,
    project_index: Option<&dyn crate::index::VectorIndex>,
) -> Result<GatherResult, AnalysisError> {
    let _span = tracing::info_span!(
        "gather_cross_index",
        ref_name = %ref_idx.name,
        query_len = query_text.len(),
        expand_depth = opts.expand_depth,
        limit = opts.limit,
    )
    .entered();

    // Model compatibility check: warn if project and reference use different embedding models
    if let (Ok(proj_model), Ok(ref_model)) = (
        project_store.get_metadata("model_name"),
        ref_idx.store.get_metadata("model_name"),
    ) {
        if proj_model != ref_model {
            tracing::warn!(
                project = %proj_model,
                reference = %ref_model,
                "Model mismatch between project and reference — results may be inaccurate"
            );
        }
    }

    // 1. Seed search against reference index (unweighted — user explicitly targets this ref)
    let filter = crate::store::helpers::SearchFilter {
        query_text: query_text.to_string(),
        enable_rrf: false, // RRF off by default — pure cosine is faster + higher R@1 on expanded eval
        ..SearchFilter::default()
    };
    let ref_seeds = crate::reference::search_reference(
        ref_idx,
        query_embedding,
        &filter,
        opts.seed_limit,
        opts.seed_threshold,
        false, // no weight for cross-index gather (user explicitly targets this ref)
    )?;
    tracing::debug!(
        ref_seed_count = ref_seeds.len(),
        "Reference seed search complete"
    );

    if ref_seeds.is_empty() {
        return Ok(GatherResult {
            chunks: Vec::new(),
            expansion_capped: false,
            search_degraded: false,
        });
    }

    // Collect ref seed chunk IDs for embedding retrieval
    let ref_seed_ids: Vec<&str> = ref_seeds.iter().map(|r| r.chunk.id.as_str()).collect();

    // 2. Get embeddings for ref seed chunks
    let ref_embeddings = match ref_idx.store.get_embeddings_by_ids(&ref_seed_ids) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to get ref seed embeddings, falling back to query embedding only");
            HashMap::new()
        }
    };

    // Build ref seed output chunks (these go into the result as reference context).
    // Use Path::new("") as root since ref files don't need path relativization.
    let ref_chunks: Vec<GatheredChunk> = ref_seeds
        .iter()
        .map(|r| {
            GatheredChunk::from_search(r, Path::new(""), r.score, 0, Some(ref_idx.name.clone()))
        })
        .collect();

    // 3. Bridge: for each ref seed, search the project store with the seed's embedding
    //    to find semantically similar project code.
    //    If no embedding available for a seed, use the original query embedding.
    //    Parallelized with rayon — Store is Send+Sync (SqlitePool + Runtime + AtomicBool).
    let bridge_filter = SearchFilter {
        query_text: query_text.to_string(),
        enable_rrf: false, // RRF off by default — pure cosine is faster + higher R@1 on expanded eval
        ..SearchFilter::default()
    };

    let bridge_limit = 3; // Top 3 project matches per ref seed

    let _bridge_span = tracing::info_span!("bridge_search", seed_count = ref_seeds.len()).entered();

    let bridge_error_count = std::sync::atomic::AtomicUsize::new(0);
    let bridge_results: Vec<(f32, Vec<SearchResult>)> = ref_seeds
        .par_iter()
        .filter_map(|seed| {
            let search_embedding = ref_embeddings
                .get(&seed.chunk.id)
                .unwrap_or(query_embedding);
            match project_store.search_filtered_with_index(
                search_embedding,
                &bridge_filter,
                bridge_limit,
                opts.seed_threshold,
                project_index,
            ) {
                Ok(r) if !r.is_empty() => Some((seed.score, r)),
                Ok(_) => None,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        ref_seed = %seed.chunk.name,
                        "Bridge search failed for ref seed"
                    );
                    bridge_error_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    None
                }
            }
        })
        .collect();

    // EH-31: Surface total bridge search failures that silently degrade quality
    let total_bridge_errors = bridge_error_count.load(std::sync::atomic::Ordering::Relaxed);
    if total_bridge_errors > 0 {
        tracing::warn!(
            failed = total_bridge_errors,
            total = ref_seeds.len(),
            "Bridge searches failed — cross-index gather quality may be reduced"
        );
    }

    drop(_bridge_span);

    // Merge into bridge_scores sequentially (HashMap not Sync)
    let mut bridge_scores: HashMap<String, (f32, String)> = HashMap::new(); // name -> (score, chunk_id)
    for (seed_score, results) in bridge_results {
        for pr in &results {
            let bridge_score = pr.score * seed_score;
            match bridge_scores.entry(pr.chunk.name.clone()) {
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert((bridge_score, pr.chunk.id.clone()));
                }
                std::collections::hash_map::Entry::Occupied(mut e) => {
                    if bridge_score > e.get().0 {
                        e.insert((bridge_score, pr.chunk.id.clone()));
                    }
                }
            }
        }
    }

    tracing::debug!(bridge_count = bridge_scores.len(), "Bridge search complete");

    if bridge_scores.is_empty() {
        // No project code found — return ref seeds only
        let mut result_chunks = ref_chunks;
        result_chunks.truncate(opts.limit);
        return Ok(GatherResult {
            chunks: result_chunks,
            expansion_capped: false,
            search_degraded: false,
        });
    }

    // 4. BFS expand project-side bridges via project call graph
    let graph = project_store.get_call_graph()?;

    let mut name_scores: HashMap<String, (f32, usize)> = HashMap::new();
    for (name, (score, _)) in &bridge_scores {
        name_scores.insert(name.clone(), (*score, 0));
    }

    let expansion_capped = bfs_expand(&mut name_scores, &graph, opts);
    tracing::debug!(
        expanded_nodes = name_scores.len(),
        expansion_capped,
        "Project BFS expansion complete"
    );

    // 5. Batch-fetch project chunks
    let (project_chunks, search_degraded) = fetch_and_assemble(project_store, &name_scores, root);

    // 6. Combine ref seeds + project chunks, sort by score, truncate, re-sort to reading order
    let mut all_chunks = ref_chunks;
    all_chunks.extend(project_chunks);

    // Sort by score desc with a full tiebreak cascade (name → file → line_start).
    // Name alone isn't unique — two chunks named `new` from different files
    // collide — so we also compare file and line_start to keep the truncate()
    // boundary deterministic across process invocations.
    all_chunks.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(a.name.cmp(&b.name))
            .then(a.file.cmp(&b.file))
            .then(a.line_start.cmp(&b.line_start))
    });
    all_chunks.truncate(opts.limit);
    // Sort: ref chunks first (by source name), then project chunks, each group in file/line order
    all_chunks.sort_by(|a, b| {
        // Reference chunks come first, project chunks second
        let source_ord = match (&a.source, &b.source) {
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            _ => std::cmp::Ordering::Equal,
        };
        source_ord
            .then(a.file.cmp(&b.file))
            .then(a.line_start.cmp(&b.line_start))
            .then(a.name.cmp(&b.name))
    });

    Ok(GatherResult {
        chunks: all_chunks,
        expansion_capped,
        search_degraded,
    })
}

/// Get neighbors in the specified direction.
/// Returns `Arc<str>` cloned from the CallGraph maps to avoid per-node String allocations (PF-1).
fn get_neighbors(graph: &CallGraph, name: &str, direction: GatherDirection) -> Vec<Arc<str>> {
    let mut neighbors = Vec::new();
    match direction {
        GatherDirection::Callees | GatherDirection::Both => {
            if let Some(callees) = graph.forward.get(name) {
                neighbors.extend(callees.iter().map(Arc::clone));
            }
        }
        _ => {}
    }
    match direction {
        GatherDirection::Callers | GatherDirection::Both => {
            if let Some(callers) = graph.reverse.get(name) {
                neighbors.extend(callers.iter().map(Arc::clone));
            }
        }
        _ => {}
    }
    neighbors
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Constructs a sample call graph representing function call relationships.
    ///
    /// Creates a CallGraph with both forward and reverse call mappings. The forward map tracks which functions each function calls, while the reverse map tracks which functions call each function.
    ///
    /// The example graph has the following structure:
    /// - Function A calls B and C
    /// - Function B calls D
    /// - Function C and D are called by A and B respectively
    ///
    /// # Returns
    ///
    /// A CallGraph containing the forward and reverse call mappings for the sample graph.
    fn make_graph() -> CallGraph {
        let mut forward = HashMap::new();
        let mut reverse = HashMap::new();

        // A calls B and C
        forward.insert("A".to_string(), vec!["B".to_string(), "C".to_string()]);
        // B calls D
        forward.insert("B".to_string(), vec!["D".to_string()]);

        // B and C are called by A
        reverse.insert("B".to_string(), vec!["A".to_string()]);
        reverse.insert("C".to_string(), vec!["A".to_string()]);
        // D is called by B
        reverse.insert("D".to_string(), vec!["B".to_string()]);

        CallGraph::from_string_maps(forward, reverse)
    }

    #[test]
    fn test_direction_parse() {
        assert!(matches!(
            "both".parse::<GatherDirection>().unwrap(),
            GatherDirection::Both
        ));
        assert!(matches!(
            "callers".parse::<GatherDirection>().unwrap(),
            GatherDirection::Callers
        ));
        assert!(matches!(
            "callees".parse::<GatherDirection>().unwrap(),
            GatherDirection::Callees
        ));
        assert!("invalid".parse::<GatherDirection>().is_err());
    }

    #[test]
    fn test_default_options() {
        let opts = GatherOptions::default();
        assert_eq!(opts.expand_depth, 1);
        assert_eq!(opts.limit, 10);
        assert!(matches!(opts.direction, GatherDirection::Both));
    }

    #[test]
    fn test_get_neighbors_callees() {
        let graph = make_graph();
        let neighbors = get_neighbors(&graph, "A", GatherDirection::Callees);
        assert_eq!(neighbors.len(), 2);
        assert!(neighbors.contains(&Arc::from("B")));
        assert!(neighbors.contains(&Arc::from("C")));
    }

    #[test]
    fn test_get_neighbors_callers() {
        let graph = make_graph();
        let neighbors = get_neighbors(&graph, "B", GatherDirection::Callers);
        assert_eq!(neighbors.len(), 1);
        assert_eq!(&*neighbors[0], "A");
    }

    #[test]
    fn test_get_neighbors_both() {
        let graph = make_graph();
        // B has callees [D] and callers [A]
        let neighbors = get_neighbors(&graph, "B", GatherDirection::Both);
        assert_eq!(neighbors.len(), 2);
        assert!(neighbors.contains(&Arc::from("D")));
        assert!(neighbors.contains(&Arc::from("A")));
    }

    #[test]
    fn test_get_neighbors_unknown_node() {
        let graph = make_graph();
        let neighbors = get_neighbors(&graph, "Z", GatherDirection::Both);
        assert!(neighbors.is_empty());
    }

    #[test]
    fn test_get_neighbors_leaf_node() {
        let graph = make_graph();
        // D has no callees, only callers
        let callees = get_neighbors(&graph, "D", GatherDirection::Callees);
        assert!(callees.is_empty());

        let callers = get_neighbors(&graph, "D", GatherDirection::Callers);
        assert_eq!(callers.len(), 1);
        assert_eq!(&*callers[0], "B");
    }

    #[test]
    fn test_gather_options_builder() {
        let opts = GatherOptions::default()
            .with_expand_depth(3)
            .with_direction(GatherDirection::Callers)
            .with_limit(20)
            .with_seed_limit(10)
            .with_seed_threshold(0.5)
            .with_decay_factor(0.9);
        assert_eq!(opts.expand_depth, 3);
        assert!(matches!(opts.direction, GatherDirection::Callers));
        assert_eq!(opts.limit, 20);
        assert_eq!(opts.seed_limit, 10);
        assert!((opts.seed_threshold - 0.5).abs() < f32::EPSILON);
        assert!((opts.decay_factor - 0.9).abs() < f32::EPSILON);
    }

    #[test]
    fn test_bfs_depth_preserves_minimum() {
        // Graph: A -> B -> D, A -> C -> D (two paths to D)
        // If A is the seed at depth 0, B and C are discovered at depth 1.
        // D is first reached via B at depth 2, then via C also at depth 2.
        // But if B has a higher score, it should update D's score without
        // overwriting D's depth to a deeper value.
        let mut forward = HashMap::new();
        let mut reverse = HashMap::new();

        forward.insert("A".to_string(), vec!["B".to_string(), "C".to_string()]);
        forward.insert("B".to_string(), vec!["D".to_string()]);
        forward.insert("C".to_string(), vec!["D".to_string()]);

        reverse.insert("B".to_string(), vec!["A".to_string()]);
        reverse.insert("C".to_string(), vec!["A".to_string()]);
        reverse.insert("D".to_string(), vec!["B".to_string(), "C".to_string()]);

        let graph = CallGraph::from_string_maps(forward, reverse);

        let mut name_scores = HashMap::new();
        name_scores.insert("A".to_string(), (1.0, 0));

        let opts = GatherOptions::default()
            .with_expand_depth(3)
            .with_direction(GatherDirection::Callees)
            .with_decay_factor(0.8);

        bfs_expand(&mut name_scores, &graph, &opts);

        // D should be discovered at depth 2 (A->B->D or A->C->D)
        // and should keep the minimum depth even if score is updated
        let (_, depth) = name_scores["D"];
        assert_eq!(depth, 2, "D should preserve minimum depth of 2");

        // B and C should be at depth 1
        assert_eq!(name_scores["B"].1, 1);
        assert_eq!(name_scores["C"].1, 1);
    }
}
