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

use crate::store::helpers::CallGraph;
use crate::Store;

/// Options for gather operation
pub struct GatherOptions {
    pub expand_depth: usize,
    pub direction: GatherDirection,
    pub limit: usize,
}

impl Default for GatherOptions {
    fn default() -> Self {
        Self {
            expand_depth: 1,
            direction: GatherDirection::Both,
            limit: 10,
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
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "both" => Ok(Self::Both),
            "callers" => Ok(Self::Callers),
            "callees" => Ok(Self::Callees),
            _ => anyhow::bail!("Invalid direction '{}'. Valid: both, callers, callees", s),
        }
    }
}

/// A gathered code chunk with context
#[derive(Debug)]
pub struct GatheredChunk {
    pub name: String,
    pub file: PathBuf,
    pub line_start: u32,
    pub line_end: u32,
    pub signature: String,
    pub content: String,
    pub score: f32,
    pub depth: usize,
}

/// Result of a gather operation
pub struct GatherResult {
    pub chunks: Vec<GatheredChunk>,
    pub expansion_capped: bool,
}

/// Maximum nodes in BFS expansion to prevent blowup on hub functions
const MAX_EXPANDED_NODES: usize = 200;

/// Gather relevant code chunks for a query
pub fn gather(
    store: &Store,
    query_embedding: &crate::Embedding,
    opts: &GatherOptions,
    project_root: &Path,
) -> Result<GatherResult> {
    // 1. Seed with search results
    let seed_results = store.search(query_embedding, 5, 0.3)?;
    if seed_results.is_empty() {
        return Ok(GatherResult {
            chunks: Vec::new(),
            expansion_capped: false,
        });
    }

    // 2. Load call graph for expansion
    let graph = store.get_call_graph()?;

    // Seed names with their scores
    let mut name_scores: HashMap<String, (f32, usize)> = HashMap::new(); // name -> (score, depth)
    for r in &seed_results {
        name_scores.insert(r.chunk.name.clone(), (r.score, 0));
    }

    // 3. BFS expand
    let mut expansion_capped = false;
    if opts.expand_depth > 0 {
        let mut queue: VecDeque<(String, usize)> = VecDeque::new();
        for r in &seed_results {
            queue.push_back((r.chunk.name.clone(), 0));
        }

        while let Some((name, depth)) = queue.pop_front() {
            if depth >= opts.expand_depth {
                continue;
            }
            if name_scores.len() >= MAX_EXPANDED_NODES {
                expansion_capped = true;
                break;
            }

            let neighbors = get_neighbors(&graph, &name, opts.direction);
            for neighbor in neighbors {
                if !name_scores.contains_key(&neighbor) {
                    // Expanded nodes get a decaying score based on depth
                    let base_score = name_scores.get(&name).map(|(s, _)| *s).unwrap_or(0.5);
                    let decay = 0.8_f32.powi((depth + 1) as i32);
                    name_scores.insert(neighbor.clone(), (base_score * decay, depth + 1));
                    queue.push_back((neighbor, depth + 1));
                }
            }
        }
    }

    // 4. Fetch chunks for all expanded names, deduplicate by id
    let mut seen_ids: HashSet<String> = HashSet::new();
    let mut chunks: Vec<GatheredChunk> = Vec::new();

    for (name, (score, depth)) in &name_scores {
        if let Ok(results) = store.search_by_name(name, 1) {
            if let Some(r) = results.into_iter().next() {
                // Dedup by chunk id
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
                    signature: r.chunk.signature.clone(),
                    content: r.chunk.content.clone(),
                    score: *score,
                    depth: *depth,
                });
            }
        }
    }

    // 5. Sort by file → line_start (reading order)
    chunks.sort_by(|a, b| a.file.cmp(&b.file).then(a.line_start.cmp(&b.line_start)));

    // 6. Truncate to limit
    chunks.truncate(opts.limit);

    Ok(GatherResult {
        chunks,
        expansion_capped,
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
