//! Suggest — auto-detect note-worthy patterns in the codebase
//!
//! Scans the index for anti-patterns (dead code clusters, untested hotspots,
//! high-risk functions) and suggests notes to add.

use std::collections::HashMap;

use anyhow::Result;

use crate::impact::find_hotspots;
use crate::{compute_risk_batch, RiskLevel, Store};

/// A suggested note from pattern detection.
#[derive(Debug)]
pub struct SuggestedNote {
    pub text: String,
    pub sentiment: f32,
    pub mentions: Vec<String>,
    /// Which detector generated this suggestion
    pub reason: String,
}

/// Scan the index for anti-patterns and suggest notes.
///
/// Each detector runs independently — if one fails, the others still produce results.
pub fn suggest_notes(store: &Store) -> Result<Vec<SuggestedNote>> {
    let _span = tracing::info_span!("suggest_notes").entered();

    let mut suggestions = Vec::new();

    // Detector 1: dead code clusters
    {
        let _span = tracing::info_span!("detect_dead_clusters").entered();
        match detect_dead_clusters(store) {
            Ok(mut s) => suggestions.append(&mut s),
            Err(e) => tracing::warn!(error = %e, "Dead code cluster detection failed"),
        }
    }

    // Detector 2: untested hotspots
    // Detector 3: high-risk functions
    // Both need call graph + test chunks, so share the data
    {
        let _span = tracing::info_span!("detect_risk_patterns").entered();
        match detect_risk_patterns(store) {
            Ok(mut s) => suggestions.append(&mut s),
            Err(e) => tracing::warn!(error = %e, "Risk pattern detection failed"),
        }
    }

    // Deduplicate against existing notes
    let existing = store.list_notes_summaries().unwrap_or_else(|e| {
        tracing::warn!(error = %e, "Failed to load existing notes for dedup");
        Vec::new()
    });

    let existing_texts: Vec<&str> = existing.iter().map(|n| n.text.as_str()).collect();
    suggestions.retain(|s| {
        !existing_texts.iter().any(|existing_text| {
            // Substring match in either direction
            existing_text.contains(&s.text) || s.text.contains(existing_text)
        })
    });

    tracing::info!(count = suggestions.len(), "Suggestions generated");
    Ok(suggestions)
}

/// Detect files with 5+ dead (uncalled) functions.
fn detect_dead_clusters(store: &Store) -> Result<Vec<SuggestedNote>> {
    let (confident, _) = store.find_dead_code(true)?;

    // Group by file
    let mut by_file: HashMap<String, usize> = HashMap::new();
    for dead in &confident {
        let file = dead.chunk.file.display().to_string();
        *by_file.entry(file).or_default() += 1;
    }

    Ok(by_file
        .into_iter()
        .filter(|(_, count)| *count >= 5)
        .map(|(file, count)| SuggestedNote {
            text: format!("{file} has {count} dead functions — consider cleanup"),
            sentiment: -0.5,
            mentions: vec![file],
            reason: "dead_code_cluster".to_string(),
        })
        .collect())
}

/// Detect untested hotspots and high-risk functions.
fn detect_risk_patterns(store: &Store) -> Result<Vec<SuggestedNote>> {
    let graph = store.get_call_graph()?;
    let test_chunks = store.find_test_chunks()?;
    let hotspots = find_hotspots(&graph, 20); // check top 20

    if hotspots.is_empty() {
        return Ok(Vec::new());
    }

    let names: Vec<&str> = hotspots.iter().map(|(n, _)| n.as_str()).collect();
    let risks = compute_risk_batch(&names, &graph, &test_chunks);

    let mut suggestions = Vec::new();

    for (risk, (name, caller_count)) in risks.iter().zip(hotspots.iter()) {
        let mentions = vec![name.to_string()];

        // Untested hotspot: 5+ callers, 0 tests
        if risk.caller_count >= 5 && risk.test_count == 0 {
            suggestions.push(SuggestedNote {
                text: format!("{name} has {caller_count} callers but no tests"),
                sentiment: -0.5,
                mentions,
                reason: "untested_hotspot".to_string(),
            });
        }
        // High-risk: many callers, few tests relative to blast radius
        else if risk.risk_level == RiskLevel::High {
            suggestions.push(SuggestedNote {
                text: format!(
                    "{name} is high-risk: {caller_count} callers, {} tests",
                    risk.test_count
                ),
                sentiment: -1.0,
                mentions,
                reason: "high_risk".to_string(),
            });
        }
    }

    Ok(suggestions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_store() -> (Store, TempDir) {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("index.db");
        let store = Store::open(&db_path).unwrap();
        store.init(&crate::store::ModelInfo::default()).unwrap();
        (store, dir)
    }

    #[test]
    fn test_suggest_empty_store() {
        let (store, _dir) = make_store();
        let suggestions = suggest_notes(&store).unwrap();
        assert!(suggestions.is_empty());
    }
}
