//! Heading extraction, hierarchy detection, and level tracking
//!
//! Scans ATX headings (`# H1` through `###### H6`), respects fenced code blocks,
//! and detects both standard (H1 > H2 > H3) and inverted (H2 title > H1 chapters)
//! hierarchies used in converted CHM/HTML documentation.

use std::collections::HashMap;

/// A detected heading in the markdown source
#[derive(Debug, Clone)]
pub(super) struct Heading {
    pub level: u32,
    pub text: String,
    pub line: usize, // 0-indexed
}

/// Scan lines for ATX headings, respecting fenced code blocks
pub(super) fn extract_headings(lines: &[&str]) -> Vec<Heading> {
    let mut headings = Vec::new();
    let mut in_code_block = false;

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();

        // Toggle fenced code block state
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_code_block = !in_code_block;
            continue;
        }

        if in_code_block {
            continue;
        }

        // ATX heading: one or more # followed by space
        if let Some(level) = atx_heading_level(trimmed) {
            let text = trimmed[level as usize..]
                .trim_start_matches(' ')
                .trim_end()
                .to_string();
            if !text.is_empty() {
                headings.push(Heading {
                    level,
                    text,
                    line: i,
                });
            }
        }
    }

    headings
}

/// Return ATX heading level (1-6) or None
pub(super) fn atx_heading_level(line: &str) -> Option<u32> {
    let bytes = line.as_bytes();
    let mut count = 0u32;
    for &b in bytes {
        if b == b'#' {
            count += 1;
        } else {
            break;
        }
    }
    // Must have 1-6 # followed by space (or line is just #s -- treat as invalid)
    if (1..=6).contains(&count) && bytes.get(count as usize) == Some(&b' ') {
        Some(count)
    } else {
        None
    }
}

/// Detect title, primary split level, and overflow split level
/// Returns (title_heading_index, primary_level, overflow_level)
pub(super) fn detect_heading_levels(headings: &[Heading]) -> (Option<usize>, u32, Option<u32>) {
    // Count frequency of each heading level
    let mut freq: HashMap<u32, usize> = HashMap::new();
    for h in headings {
        *freq.entry(h.level).or_insert(0) += 1;
    }

    // Title level: level of the first heading
    let first_level = headings[0].level;
    let first_level_count = freq.get(&first_level).copied().unwrap_or(0);

    // Title index: first heading, but only if its level appears once
    // (or if it's the shallowest level and appears first)
    let title_idx = if first_level_count == 1 {
        Some(0)
    } else {
        // First heading's level appears multiple times -- no distinct title
        None
    };

    // Primary split level: shallowest heading level appearing more than once,
    // excluding the title level if it only appears once
    let mut levels: Vec<u32> = freq.keys().copied().collect();
    levels.sort();

    let primary_level = levels
        .iter()
        .copied()
        .find(|&lvl| {
            let count = freq.get(&lvl).copied().unwrap_or(0);
            if title_idx.is_some() && lvl == first_level {
                false // Skip title level
            } else {
                count > 1
            }
        })
        .unwrap_or(first_level); // Fallback: split at first heading's level

    // Overflow level: next level deeper than primary that exists
    // (excluding the title level -- it's a parent, not a subsection)
    let title_level = title_idx.map(|i| headings[i].level);
    let overflow_level = levels
        .iter()
        .copied()
        .find(|&lvl| lvl > primary_level && Some(lvl) != title_level);

    (title_idx, primary_level, overflow_level)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_atx_heading_level() {
        assert_eq!(atx_heading_level("# Title"), Some(1));
        assert_eq!(atx_heading_level("## Section"), Some(2));
        assert_eq!(atx_heading_level("### Sub"), Some(3));
        assert_eq!(atx_heading_level("###### Deep"), Some(6));
        assert_eq!(atx_heading_level("####### Too deep"), None);
        assert_eq!(atx_heading_level("#NoSpace"), None);
        assert_eq!(atx_heading_level("Not a heading"), None);
        assert_eq!(atx_heading_level(""), None);
    }

    #[test]
    fn test_headings_in_code_blocks_ignored() {
        let source =
            "# Real heading\n\n```\n# Not a heading\n## Also not\n```\n\n## Another real heading\n";
        let lines: Vec<&str> = source.lines().collect();
        let headings = extract_headings(&lines);
        assert_eq!(headings.len(), 2);
        assert_eq!(headings[0].text, "Real heading");
        assert_eq!(headings[1].text, "Another real heading");
    }

    #[test]
    fn test_detect_levels_standard() {
        let headings = vec![
            Heading {
                level: 1,
                text: "Title".into(),
                line: 0,
            },
            Heading {
                level: 2,
                text: "A".into(),
                line: 5,
            },
            Heading {
                level: 2,
                text: "B".into(),
                line: 20,
            },
            Heading {
                level: 3,
                text: "Sub".into(),
                line: 30,
            },
        ];
        let (title_idx, primary, overflow) = detect_heading_levels(&headings);
        assert_eq!(title_idx, Some(0));
        assert_eq!(primary, 2);
        assert_eq!(overflow, Some(3));
    }

    #[test]
    fn test_detect_levels_inverted() {
        let headings = vec![
            Heading {
                level: 2,
                text: "Doc Title".into(),
                line: 0,
            },
            Heading {
                level: 1,
                text: "Chapter A".into(),
                line: 10,
            },
            Heading {
                level: 1,
                text: "Chapter B".into(),
                line: 50,
            },
            Heading {
                level: 3,
                text: "Sub".into(),
                line: 60,
            },
        ];
        let (title_idx, primary, overflow) = detect_heading_levels(&headings);
        assert_eq!(title_idx, Some(0));
        assert_eq!(primary, 1);
        assert_eq!(overflow, Some(3));
    }
}
