//! Hunch parsing and types
//!
//! Parses hunches.toml files into structured Hunch entries.
//! Hunches are soft observations and latent risks that surface in search results.

use chrono::NaiveDate;
use serde::Deserialize;
use std::path::Path;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum HunchError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("TOML parse error: {0}")]
    Toml(#[from] toml::de::Error),
}

/// Severity of a hunch (how bad if true and ignored)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    High,
    #[default]
    Med,
    Low,
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Severity::High => write!(f, "high"),
            Severity::Med => write!(f, "med"),
            Severity::Low => write!(f, "low"),
        }
    }
}

impl std::str::FromStr for Severity {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "high" => Ok(Severity::High),
            "med" | "medium" => Ok(Severity::Med),
            "low" => Ok(Severity::Low),
            _ => anyhow::bail!("Unknown severity: '{}'. Valid: high, med, low", s),
        }
    }
}

/// Confidence in the hunch (how sure you are)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    High,
    #[default]
    Med,
    Low,
}

impl std::fmt::Display for Confidence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Confidence::High => write!(f, "high"),
            Confidence::Med => write!(f, "med"),
            Confidence::Low => write!(f, "low"),
        }
    }
}

impl std::str::FromStr for Confidence {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "high" => Ok(Confidence::High),
            "med" | "medium" => Ok(Confidence::Med),
            "low" => Ok(Confidence::Low),
            _ => anyhow::bail!("Unknown confidence: '{}'. Valid: high, med, low", s),
        }
    }
}

/// Resolution status of a hunch
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Resolution {
    #[default]
    Open,
    Resolved,
    Accepted,
}

impl std::fmt::Display for Resolution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Resolution::Open => write!(f, "open"),
            Resolution::Resolved => write!(f, "resolved"),
            Resolution::Accepted => write!(f, "accepted"),
        }
    }
}

impl std::str::FromStr for Resolution {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "open" => Ok(Resolution::Open),
            "resolved" => Ok(Resolution::Resolved),
            "accepted" => Ok(Resolution::Accepted),
            _ => anyhow::bail!(
                "Unknown resolution: '{}'. Valid: open, resolved, accepted",
                s
            ),
        }
    }
}

/// Raw hunch entry from TOML
#[derive(Debug, Deserialize)]
struct HunchEntry {
    date: String,
    title: String,
    description: String,
    #[serde(default)]
    severity: Severity,
    #[serde(default)]
    confidence: Confidence,
    #[serde(default)]
    resolution: Resolution,
    #[serde(default)]
    mentions: Vec<String>,
}

/// TOML file structure
#[derive(Debug, Deserialize)]
struct HunchFile {
    #[serde(default)]
    hunch: Vec<HunchEntry>,
}

/// A parsed hunch entry
#[derive(Debug, Clone)]
pub struct Hunch {
    /// Unique identifier: "hunch:{date}-{slug}"
    pub id: String,
    /// Date the hunch was recorded
    pub date: NaiveDate,
    /// Short title
    pub title: String,
    /// Full description (embedded for semantic search)
    pub description: String,
    /// How bad if true and ignored
    pub severity: Severity,
    /// How confident in the hunch
    pub confidence: Confidence,
    /// Resolution status
    pub resolution: Resolution,
    /// Code paths/functions mentioned (for linking)
    pub mentions: Vec<String>,
}

impl Hunch {
    /// Generate embedding text for this hunch
    ///
    /// Combines title and description into a searchable string.
    pub fn embedding_text(&self) -> String {
        format!("{}: {}", self.title, self.description)
    }
}

/// Parse hunches from a hunches.toml file
///
/// Expected format:
/// ```toml
/// [[hunch]]
/// date = "2026-01-31"
/// title = "ONNX model assumptions"
/// severity = "high"
/// confidence = "high"
/// mentions = ["embedder.rs", "load_model"]
/// description = """
/// nomic-embed-text-v1.5 ONNX model needs i64 inputs...
/// """
///
/// [[hunch]]
/// date = "2026-01-31"
/// title = "Another observation"
/// description = "Something to watch."
/// ```
pub fn parse_hunches(path: &Path) -> Result<Vec<Hunch>, HunchError> {
    let content = std::fs::read_to_string(path)?;
    parse_hunches_str(&content)
}

/// Parse hunches from a string (for testing)
pub fn parse_hunches_str(content: &str) -> Result<Vec<Hunch>, HunchError> {
    let file: HunchFile = toml::from_str(content)?;

    let hunches = file
        .hunch
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

            let id = format!("hunch:{}-{}", date.format("%Y-%m-%d"), slug);

            Hunch {
                id,
                date,
                title: entry.title,
                description: entry.description.trim().to_string(),
                severity: entry.severity,
                confidence: entry.confidence,
                resolution: entry.resolution,
                mentions: entry.mentions,
            }
        })
        .collect();

    Ok(hunches)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_toml() {
        let content = r#"
[[hunch]]
date = "2026-01-31"
title = "ONNX model assumptions"
severity = "high"
confidence = "high"
mentions = ["embedder.rs", "load_model"]
resolution = "resolved"
description = """
nomic-embed-text-v1.5 needs i64 inputs.
"""

[[hunch]]
date = "2026-01-31"
title = "Pool size tuning"
description = "May need adjustment."
"#;

        let hunches = parse_hunches_str(content).unwrap();
        assert_eq!(hunches.len(), 2);

        assert_eq!(hunches[0].title, "ONNX model assumptions");
        assert_eq!(hunches[0].severity, Severity::High);
        assert_eq!(hunches[0].confidence, Confidence::High);
        assert_eq!(hunches[0].resolution, Resolution::Resolved);
        assert_eq!(hunches[0].mentions, vec!["embedder.rs", "load_model"]);

        assert_eq!(hunches[1].title, "Pool size tuning");
        assert_eq!(hunches[1].severity, Severity::Med); // default
        assert_eq!(hunches[1].resolution, Resolution::Open); // default
    }

    #[test]
    fn test_hunch_id_generation() {
        let content = r#"
[[hunch]]
date = "2026-01-31"
title = "FTS5 tokenization needs preprocessing"
description = "Something."
"#;

        let hunches = parse_hunches_str(content).unwrap();
        assert_eq!(
            hunches[0].id,
            "hunch:2026-01-31-fts5-tokenization-needs-preprocessing"
        );
    }

    #[test]
    fn test_empty_file() {
        let content = "# Just a comment\n";
        let hunches = parse_hunches_str(content).unwrap();
        assert!(hunches.is_empty());
    }
}
