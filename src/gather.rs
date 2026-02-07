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

    // 5. Sort by score desc, truncate to limit, then re-sort by file → line_start
    chunks.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    chunks.truncate(opts.limit);
    chunks.sort_by(|a, b| a.file.cmp(&b.file).then(a.line_start.cmp(&b.line_start)));

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
}
