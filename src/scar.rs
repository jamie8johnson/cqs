//! Scar parsing and types
//!
//! Parses scars.toml files into structured Scar entries.
//! Scars are failed approaches - limbic memory that surfaces when relevant.

use chrono::NaiveDate;
use serde::Deserialize;
use std::path::Path;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ScarError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("TOML parse error: {0}")]
    Toml(#[from] toml::de::Error),
}

/// Raw scar entry from TOML
#[derive(Debug, Deserialize)]
struct ScarEntry {
    date: String,
    title: String,
    tried: String,
    pain: String,
    learned: String,
    #[serde(default)]
    mentions: Vec<String>,
}

/// TOML file structure
#[derive(Debug, Deserialize)]
struct ScarFile {
    #[serde(default)]
    scar: Vec<ScarEntry>,
}

/// A parsed scar entry
#[derive(Debug, Clone)]
pub struct Scar {
    /// Unique identifier: "scar:{date}-{slug}"
    pub id: String,
    /// Date the scar was recorded
    pub date: NaiveDate,
    /// Short title
    pub title: String,
    /// What was attempted
    pub tried: String,
    /// What hurt
    pub pain: String,
    /// What to do instead
    pub learned: String,
    /// Code paths/functions mentioned (for linking)
    pub mentions: Vec<String>,
}

impl Scar {
    /// Generate embedding text for this scar
    ///
    /// Combines all fields into a searchable string.
    pub fn embedding_text(&self) -> String {
        format!(
            "{}: Tried: {} Pain: {} Learned: {}",
            self.title, self.tried, self.pain, self.learned
        )
    }
}

/// Parse scars from a scars.toml file
pub fn parse_scars(path: &Path) -> Result<Vec<Scar>, ScarError> {
    let content = std::fs::read_to_string(path)?;
    parse_scars_str(&content)
}

/// Parse scars from a string (for testing)
pub fn parse_scars_str(content: &str) -> Result<Vec<Scar>, ScarError> {
    let file: ScarFile = toml::from_str(content)?;

    let scars = file
        .scar
        .into_iter()
        .map(|entry| {
            let date = NaiveDate::parse_from_str(&entry.date, "%Y-%m-%d")
                .unwrap_or_else(|_| NaiveDate::from_ymd_opt(2000, 1, 1).unwrap());

            // Generate ID from date and title slug
            let slug: String = entry
                .title
                .to_lowercase()
                .chars()
                .map(|c| if c.is_alphanumeric() { c } else { '-' })
                .collect::<String>()
                .split('-')
                .filter(|s| !s.is_empty())
                .take(5)
                .collect::<Vec<_>>()
                .join("-");

            let id = format!("scar:{}-{}", date.format("%Y-%m-%d"), slug);

            Scar {
                id,
                date,
                title: entry.title,
                tried: entry.tried.trim().to_string(),
                pain: entry.pain.trim().to_string(),
                learned: entry.learned.trim().to_string(),
                mentions: entry.mentions,
            }
        })
        .collect();

    Ok(scars)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_toml() {
        let content = r#"
[[scar]]
date = "2026-01-31"
title = "tree-sitter grammar mismatch"
mentions = ["tree-sitter", "parser.rs"]
tried = "Using tree-sitter 0.26 with grammar crates at 0.23.x"
pain = "Mysterious parsing failures."
learned = "Keep grammar versions aligned."
"#;

        let scars = parse_scars_str(content).unwrap();
        assert_eq!(scars.len(), 1);
        assert_eq!(scars[0].title, "tree-sitter grammar mismatch");
        assert!(scars[0].tried.contains("0.26"));
        assert!(scars[0].learned.contains("aligned"));
    }

    #[test]
    fn test_scar_id_generation() {
        let content = r#"
[[scar]]
date = "2026-01-31"
title = "MCP tools/call returning raw JSON"
tried = "x"
pain = "y"
learned = "z"
"#;

        let scars = parse_scars_str(content).unwrap();
        assert_eq!(scars[0].id, "scar:2026-01-31-mcp-tools-call-returning-raw");
    }

    #[test]
    fn test_empty_file() {
        let content = "# Just a comment\n";
        let scars = parse_scars_str(content).unwrap();
        assert!(scars.is_empty());
    }
}
