//! Suggest — auto-detect note-worthy patterns in the codebase
//!
//! Scans the index for anti-patterns (dead code clusters, untested hotspots,
//! high-risk functions, stale note mentions) and suggests notes to add.

use std::collections::HashMap;
use std::path::Path;

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
pub fn suggest_notes(store: &Store, project_root: &Path) -> Result<Vec<SuggestedNote>> {
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

    // Detector 4: stale note mentions
    {
        let _span = tracing::info_span!("detect_stale_mentions").entered();
        match detect_stale_mentions(store, project_root) {
            Ok(mut s) => suggestions.append(&mut s),
            Err(e) => tracing::warn!(error = %e, "Stale mention detection failed"),
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

// ─── Mention classification ──────────────────────────────────────────────────

/// How a note mention should be verified.
#[derive(Debug, PartialEq)]
enum MentionKind {
    /// Contains `.` or `/` — check filesystem
    File,
    /// Contains `_` or `::` or is PascalCase — check index
    Symbol,
    /// Everything else — not verifiable
    Concept,
}

/// Classify a mention string for staleness checking.
fn classify_mention(mention: &str) -> MentionKind {
    if mention.contains('.') || mention.contains('/') || mention.contains('\\') {
        MentionKind::File
    } else if mention.contains('_') || mention.contains("::") || is_pascal_case(mention) {
        MentionKind::Symbol
    } else {
        MentionKind::Concept
    }
}

/// Check if a string is PascalCase (starts uppercase, has lowercase chars, len > 1).
fn is_pascal_case(s: &str) -> bool {
    s.len() > 1
        && s.chars().next().is_some_and(|c| c.is_uppercase())
        && s.chars().any(|c| c.is_lowercase())
}

/// Core logic: find stale mentions across all notes.
///
/// Returns `(note_text, stale_mentions)` pairs for each note with at least one
/// stale mention. Shared by `detect_stale_mentions` and `check_note_staleness`.
fn find_stale_mentions(store: &Store, project_root: &Path) -> Result<Vec<(String, Vec<String>)>> {
    let notes = store.list_notes_summaries()?;

    // Batch all symbol mentions for one query
    let mut symbol_mentions: Vec<&str> = Vec::new();
    for note in &notes {
        for mention in &note.mentions {
            if matches!(classify_mention(mention), MentionKind::Symbol) {
                symbol_mentions.push(mention.as_str());
            }
        }
    }
    symbol_mentions.sort_unstable();
    symbol_mentions.dedup();

    let symbol_results = if symbol_mentions.is_empty() {
        HashMap::new()
    } else {
        store.search_by_names_batch(&symbol_mentions, 1)?
    };

    let mut result = Vec::new();

    for note in &notes {
        let mut stale = Vec::new();
        for mention in &note.mentions {
            match classify_mention(mention) {
                MentionKind::File => {
                    if !project_root.join(mention).exists() {
                        stale.push(mention.clone());
                    }
                }
                MentionKind::Symbol => {
                    if symbol_results
                        .get(mention.as_str())
                        .is_none_or(|v| v.is_empty())
                    {
                        stale.push(mention.clone());
                    }
                }
                MentionKind::Concept => {} // skip — not verifiable
            }
        }
        if !stale.is_empty() {
            result.push((note.text.clone(), stale));
        }
    }

    Ok(result)
}

/// Detect notes with stale mentions (deleted files, removed functions).
fn detect_stale_mentions(store: &Store, project_root: &Path) -> Result<Vec<SuggestedNote>> {
    let stale_pairs = find_stale_mentions(store, project_root)?;

    Ok(stale_pairs
        .into_iter()
        .map(|(text, stale)| {
            let preview = if text.len() > 80 {
                format!("{}...", &text[..text.floor_char_boundary(77)])
            } else {
                text
            };
            SuggestedNote {
                text: format!(
                    "Note has stale mentions [{}]: \"{}\"",
                    stale.join(", "),
                    preview,
                ),
                sentiment: -0.5,
                mentions: stale,
                reason: "stale_mention".to_string(),
            }
        })
        .collect())
}

/// Check all notes for stale mentions.
///
/// Returns `(note_text, stale_mentions)` pairs for each note that has at least
/// one stale mention. Reusable by `notes list --check` and future `health` integration.
pub fn check_note_staleness(
    store: &Store,
    project_root: &Path,
) -> Result<Vec<(String, Vec<String>)>> {
    let _span = tracing::info_span!("check_note_staleness").entered();
    let result = find_stale_mentions(store, project_root)?;
    tracing::info!(stale_notes = result.len(), "Note staleness check complete");
    Ok(result)
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
        let (store, dir) = make_store();
        let suggestions = suggest_notes(&store, dir.path()).unwrap();
        assert!(suggestions.is_empty());
    }

    #[test]
    fn test_classify_mention_file() {
        assert_eq!(classify_mention("src/foo.rs"), MentionKind::File);
        assert_eq!(classify_mention("Cargo.toml"), MentionKind::File);
        assert_eq!(classify_mention("path/to/file"), MentionKind::File);
    }

    #[test]
    fn test_classify_mention_symbol() {
        assert_eq!(classify_mention("search_filtered"), MentionKind::Symbol);
        assert_eq!(classify_mention("Store::open"), MentionKind::Symbol);
        assert_eq!(classify_mention("CallGraph"), MentionKind::Symbol);
    }

    #[test]
    fn test_classify_mention_concept() {
        assert_eq!(classify_mention("error handling"), MentionKind::Concept);
        assert_eq!(classify_mention("tree-sitter"), MentionKind::Concept);
        assert_eq!(classify_mention("indexing"), MentionKind::Concept);
    }

    #[test]
    fn test_is_pascal_case() {
        assert!(is_pascal_case("CallGraph"));
        assert!(is_pascal_case("Store"));
        assert!(!is_pascal_case("store"));
        assert!(!is_pascal_case("ALLCAPS"));
        assert!(!is_pascal_case("A")); // too short
    }

    #[test]
    fn test_detect_stale_file_mention() {
        let (store, dir) = make_store();
        // Insert a note with a mention of a nonexistent file
        store
            .replace_notes_for_file(
                &[(
                    crate::note::Note {
                        id: "note:test1".to_string(),
                        text: "test note".to_string(),
                        sentiment: 0.0,
                        mentions: vec!["src/nonexistent.rs".to_string()],
                    },
                    crate::Embedding::new(vec![0.0; 769]),
                )],
                &dir.path().join("notes.toml"),
                0,
            )
            .unwrap();

        let stale = detect_stale_mentions(&store, dir.path()).unwrap();
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].reason, "stale_mention");
        assert!(stale[0]
            .mentions
            .contains(&"src/nonexistent.rs".to_string()));
    }

    #[test]
    fn test_detect_stale_no_mentions() {
        let (store, dir) = make_store();
        // Insert a note with no mentions
        store
            .replace_notes_for_file(
                &[(
                    crate::note::Note {
                        id: "note:test2".to_string(),
                        text: "no mentions here".to_string(),
                        sentiment: 0.0,
                        mentions: vec![],
                    },
                    crate::Embedding::new(vec![0.0; 769]),
                )],
                &dir.path().join("notes.toml"),
                0,
            )
            .unwrap();

        let stale = detect_stale_mentions(&store, dir.path()).unwrap();
        assert!(stale.is_empty());
    }
}
