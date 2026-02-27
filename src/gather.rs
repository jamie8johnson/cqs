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

use anyhow::Result;
use rayon::prelude::*;

use crate::parser::{ChunkType, Language};

use crate::store::helpers::{CallGraph, SearchFilter};
use crate::store::SearchResult;
use crate::Store;

/// Default maximum nodes in BFS expansion to prevent blowup on hub functions.
pub const DEFAULT_MAX_EXPANDED_NODES: usize = 200;

/// Options for gather operation
#[derive(Debug)]
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
}

impl GatherOptions {
    pub fn with_expand_depth(mut self, depth: usize) -> Self {
        self.expand_depth = depth;
        self
    }
    pub fn with_direction(mut self, direction: GatherDirection) -> Self {
        self.direction = direction;
        self
    }
    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }
    pub fn with_seed_limit(mut self, limit: usize) -> Self {
        self.seed_limit = limit;
        self
    }
    pub fn with_seed_threshold(mut self, threshold: f32) -> Self {
        self.seed_threshold = threshold;
        self
    }
    pub fn with_decay_factor(mut self, factor: f32) -> Self {
        self.decay_factor = if factor.is_finite() {
            factor.clamp(0.0, 1.0)
        } else {
            self.decay_factor
        };
        self
    }
    pub fn with_max_expanded_nodes(mut self, max: usize) -> Self {
        self.max_expanded_nodes = max;
        self
    }
}

impl Default for GatherOptions {
    fn default() -> Self {
        Self {
            expand_depth: 1,
            direction: GatherDirection::Both,
            limit: 10,
            seed_limit: 5,
            seed_threshold: 0.3,
            decay_factor: 0.8,
            max_expanded_nodes: DEFAULT_MAX_EXPANDED_NODES,
        }
    }
}

/// Direction of call graph expansion
#[derive(Debug, Clone, Copy)]
pub enum GatherDirection {
    Both,
    Callers,
    Callees,
}

impl std::str::FromStr for GatherDirection {
    type Err = String;
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
#[derive(Debug, Clone)]
pub struct GatheredChunk {
    pub name: String,
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
    pub source: Option<String>,
}

impl GatheredChunk {
    /// Serialize to JSON, relativizing file paths against the project root.
    pub fn to_json(&self, root: &Path) -> serde_json::Value {
        serde_json::json!({
            "name": self.name,
            "file": crate::rel_display(&self.file, root),
            "line_start": self.line_start,
            "line_end": self.line_end,
            "language": self.language.to_string(),
            "chunk_type": self.chunk_type.to_string(),
            "signature": self.signature,
            "content": self.content,
            "score": self.score,
            "depth": self.depth,
        })
    }
}

/// Result of a gather operation
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

    let mut queue: VecDeque<(String, usize)> = VecDeque::new();
    for (name, &(_, depth)) in name_scores.iter() {
        queue.push_back((name.clone(), depth));
    }

    while let Some((name, depth)) = queue.pop_front() {
        if depth >= opts.expand_depth {
            continue;
        }
        if name_scores.len() >= opts.max_expanded_nodes {
            expansion_capped = true;
            break;
        }

        let neighbors = get_neighbors(graph, &name, opts.direction);
        let base_score = name_scores.get(&name).map(|(s, _)| *s).unwrap_or(0.5);
        let new_score = base_score * opts.decay_factor;
        for neighbor in neighbors {
            if name_scores.len() >= opts.max_expanded_nodes {
                expansion_capped = true;
                break;
            }
            match name_scores.entry(neighbor.clone()) {
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert((new_score, depth + 1));
                    queue.push_back((neighbor, depth + 1));
                }
                std::collections::hash_map::Entry::Occupied(mut e) => {
                    if new_score > e.get().0 {
                        e.insert((new_score, depth + 1));
                        // Don't re-add to queue — already explored or queued
                    }
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
    project_root: &Path,
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

                chunks.push(GatheredChunk {
                    name: r.chunk.name.clone(),
                    file: r
                        .chunk
                        .file
                        .strip_prefix(project_root)
                        .unwrap_or(&r.chunk.file)
                        .to_path_buf(),
                    line_start: r.chunk.line_start,
                    line_end: r.chunk.line_end,
                    language: r.chunk.language,
                    chunk_type: r.chunk.chunk_type,
                    signature: r.chunk.signature.clone(),
                    content: r.chunk.content.clone(),
                    score: *score,
                    depth: *depth,
                    source: None,
                });
            }
        }
    }

    tracing::debug!(chunk_count = chunks.len(), "Chunks assembled");
    (chunks, search_degraded)
}

/// Sort chunks by score desc (name tiebreak), truncate to limit,
/// then re-sort to file/line reading order.
pub(crate) fn sort_and_truncate(chunks: &mut Vec<GatheredChunk>, limit: usize) {
    chunks.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.name.cmp(&b.name)));
    chunks.truncate(limit);
    chunks.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then(a.line_start.cmp(&b.line_start))
            .then(a.name.cmp(&b.name))
    });
}

/// Gather relevant code chunks for a query
pub fn gather(
    store: &Store,
    query_embedding: &crate::Embedding,
    query_text: &str,
    opts: &GatherOptions,
    project_root: &Path,
) -> Result<GatherResult> {
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
        enable_rrf: true,
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

    // 2. Load call graph for expansion
    // NOTE: This loads the entire function_calls table into memory each call.
    // Current callers invoke gather() once per request so this is fine.
    // If gather() is ever called in a loop, accept a pre-loaded &CallGraph parameter
    // to avoid redundant loads.
    let graph = store.get_call_graph()?;

    // Seed names with their scores
    let mut name_scores: HashMap<String, (f32, usize)> = HashMap::new();
    for r in &seed_results {
        name_scores.insert(r.chunk.name.clone(), (r.score, 0));
    }

    // 3. BFS expand
    let expansion_capped = bfs_expand(&mut name_scores, &graph, opts);
    tracing::debug!(
        expanded_nodes = name_scores.len(),
        expansion_capped,
        "BFS expansion complete"
    );

    // 4. Batch-fetch chunks, deduplicate
    let (mut chunks, search_degraded) = fetch_and_assemble(store, &name_scores, project_root);

    // 5. Sort by score desc, truncate to limit, re-sort to reading order
    sort_and_truncate(&mut chunks, opts.limit);

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
    project_root: &Path,
) -> Result<GatherResult> {
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
        enable_rrf: true,
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

    // Build ref seed output chunks (these go into the result as reference context)
    let ref_chunks: Vec<GatheredChunk> = ref_seeds
        .iter()
        .map(|r| GatheredChunk {
            name: r.chunk.name.clone(),
            file: r.chunk.file.clone(),
            line_start: r.chunk.line_start,
            line_end: r.chunk.line_end,
            language: r.chunk.language,
            chunk_type: r.chunk.chunk_type,
            signature: r.chunk.signature.clone(),
            content: r.chunk.content.clone(),
            score: r.score,
            depth: 0,
            source: Some(ref_idx.name.clone()),
        })
        .collect();

    // 3. Bridge: for each ref seed, search the project store with the seed's embedding
    //    to find semantically similar project code.
    //    If no embedding available for a seed, use the original query embedding.
    //    Parallelized with rayon — Store is Send+Sync (SqlitePool + Runtime + AtomicBool).
    let bridge_filter = SearchFilter {
        query_text: query_text.to_string(),
        enable_rrf: true,
        ..SearchFilter::default()
    };

    let bridge_limit = 3; // Top 3 project matches per ref seed

    let _bridge_span = tracing::info_span!("bridge_search", seed_count = ref_seeds.len()).entered();

    let bridge_results: Vec<(f32, Vec<SearchResult>)> = ref_seeds
        .par_iter()
        .filter_map(|seed| {
            let search_embedding = ref_embeddings
                .get(&seed.chunk.id)
                .unwrap_or(query_embedding);
            match project_store.search_filtered(
                search_embedding,
                &bridge_filter,
                bridge_limit,
                opts.seed_threshold,
            ) {
                Ok(r) if !r.is_empty() => Some((seed.score, r)),
                Ok(_) => None,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        ref_seed = %seed.chunk.name,
                        "Bridge search failed for ref seed"
                    );
                    None
                }
            }
        })
        .collect();

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
    let (project_chunks, search_degraded) =
        fetch_and_assemble(project_store, &name_scores, project_root);

    // 6. Combine ref seeds + project chunks, sort by score, truncate, re-sort to reading order
    let mut all_chunks = ref_chunks;
    all_chunks.extend(project_chunks);

    all_chunks.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.name.cmp(&b.name)));
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

/// Get neighbors in the specified direction
fn get_neighbors(graph: &CallGraph, name: &str, direction: GatherDirection) -> Vec<String> {
    let mut neighbors = Vec::new();
    match direction {
        GatherDirection::Callees | GatherDirection::Both => {
            if let Some(callees) = graph.forward.get(name) {
                neighbors.extend(callees.iter().cloned());
            }
        }
        _ => {}
    }
    match direction {
        GatherDirection::Callers | GatherDirection::Both => {
            if let Some(callers) = graph.reverse.get(name) {
                neighbors.extend(callers.iter().cloned());
            }
        }
        _ => {}
    }
    neighbors
}

#[cfg(test)]
mod tests {
    use super::*;

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

        CallGraph { forward, reverse }
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
        assert!(neighbors.contains(&"B".to_string()));
        assert!(neighbors.contains(&"C".to_string()));
    }

    #[test]
    fn test_get_neighbors_callers() {
        let graph = make_graph();
        let neighbors = get_neighbors(&graph, "B", GatherDirection::Callers);
        assert_eq!(neighbors.len(), 1);
        assert_eq!(neighbors[0], "A");
    }

    #[test]
    fn test_get_neighbors_both() {
        let graph = make_graph();
        // B has callees [D] and callers [A]
        let neighbors = get_neighbors(&graph, "B", GatherDirection::Both);
        assert_eq!(neighbors.len(), 2);
        assert!(neighbors.contains(&"D".to_string()));
        assert!(neighbors.contains(&"A".to_string()));
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
        assert_eq!(callers[0], "B");
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
}
