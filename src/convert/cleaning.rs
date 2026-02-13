//! Extensible cleaning rules for document-to-Markdown conversion artifacts.
//!
//! Each rule is a tagged, self-contained function that transforms lines of Markdown.
//! Rules are filtered by tags, so vendor-specific rules only run when requested.
//!
//! ## Adding new rules
//!
//! 1. Write a function with signature `fn(&mut Vec<String>, &CleaningContext) -> usize`
//! 2. Add a `CleaningRule` entry to `ALL_RULES`
//! 3. Tag it appropriately (e.g., `["siemens", "pdf"]`, `["generic"]`)
//!
//! Users control which rules run via `--clean-tags` (default: all).

use std::sync::LazyLock;

use regex::Regex;

// ============ Compiled Regexes (LazyLock) ============

/// Matches copyright lines like `© 2015-2024 by AVEVA Group Limited` with any year range.
static COPYRIGHT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)©\s*\d{4}[-\u{2013}]\d{4}.*AVEVA Group Limited")
        .expect("hardcoded copyright regex")
});

/// Matches `softwaresupport.aveva.com` in copyright boilerplate.
static SUPPORT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"softwaresupport\.aveva\.com").expect("hardcoded support regex"));

/// Matches `Page N` lines in PDF page boundaries.
static PAGE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^Page\s+\d+\s*$").expect("hardcoded page regex"));

/// Matches `©` at start of line in PDF page footers.
static PAGE_COPYRIGHT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^©").expect("hardcoded page copyright regex"));

/// Matches bare `Chapter N` headings.
static CHAPTER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^#{1,6}\s+Chapter\s+\d+\s*$").expect("hardcoded chapter regex"));

/// Matches `~~text~~` strikethrough markers.
static STRIKE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"~~([^~]+)~~").expect("hardcoded strikethrough regex"));

/// Context available to cleaning rules during the pipeline.
pub struct CleaningContext {
    pub doc_title: String,
}

/// A cleaning rule that transforms Markdown lines.
pub struct CleaningRule {
    pub name: &'static str,
    pub description: &'static str,
    /// Tags for enabling/disabling groups of rules.
    pub tags: &'static [&'static str],
    pub apply: fn(&mut Vec<String>, &CleaningContext) -> usize,
}

/// All registered cleaning rules, applied in order.
static ALL_RULES: &[CleaningRule] = &[
    CleaningRule {
        name: "copyright_boilerplate",
        description: "Remove AVEVA copyright header block",
        tags: &["aveva", "pdf"],
        apply: rule_copyright_boilerplate,
    },
    CleaningRule {
        name: "page_boundaries",
        description: "Remove Page N + title echo blocks from PDF conversion",
        tags: &["aveva", "pdf"],
        apply: rule_page_boundaries,
    },
    CleaningRule {
        name: "toc_section",
        description: "Remove # Contents through next H1",
        tags: &["generic", "pdf"],
        apply: rule_toc_section,
    },
    CleaningRule {
        name: "chapter_headings",
        description: "Strip bare 'Chapter N' headings",
        tags: &["generic", "pdf"],
        apply: rule_chapter_headings,
    },
    CleaningRule {
        name: "bold_bullets",
        description: "Replace **•** with -",
        tags: &["generic", "pdf"],
        apply: rule_bold_bullets,
    },
    CleaningRule {
        name: "strikethrough",
        description: "Remove ~~text~~ strikethrough markers",
        tags: &["generic"],
        apply: rule_strikethrough,
    },
    CleaningRule {
        name: "blank_lines",
        description: "Collapse 3+ consecutive blank lines to 2",
        tags: &["generic"],
        apply: rule_blank_lines,
    },
];

/// Run all enabled cleaning rules in order.
///
/// If `tags` is empty, all rules are applied. Otherwise only rules
/// matching at least one of the given tags are applied.
pub fn clean_markdown(input: &str, tags: &[&str]) -> String {
    let _span = tracing::info_span!("clean_markdown").entered();

    let mut lines: Vec<String> = input.lines().map(|l| l.to_string()).collect();
    let mut ctx = CleaningContext {
        doc_title: String::new(),
    };

    for rule in ALL_RULES {
        // Skip rules that don't match any requested tag
        if !tags.is_empty() && !rule.tags.iter().any(|t| tags.contains(t)) {
            continue;
        }

        let count = (rule.apply)(&mut lines, &ctx);
        if count > 0 {
            tracing::info!(rule = rule.name, changes = count, "Cleaning rule applied");
        }

        // Update context after copyright removal (title is now discoverable)
        if rule.name == "copyright_boilerplate" {
            ctx.doc_title = extract_doc_title(&lines);
        }
    }

    let mut result = lines.join("\n");
    if !result.ends_with('\n') {
        result.push('\n');
    }
    result
}

/// Extract the first H1 title from lines (used for page boundary detection).
fn extract_doc_title(lines: &[String]) -> String {
    for line in lines {
        let trimmed = line.trim();
        if let Some(heading) = trimmed.strip_prefix("# ") {
            if !heading.starts_with('#') {
                return heading.trim().to_string();
            }
        }
    }
    String::new()
}

// ============ Rule Implementations ============

/// Rule 1: Remove AVEVA copyright boilerplate from the start of the file.
///
/// Detects `© YYYY-YYYY ... AVEVA Group Limited` (any year range) in the first 80 lines,
/// removes everything up to and including the `softwaresupport.aveva.com` line.
fn rule_copyright_boilerplate(lines: &mut Vec<String>, _ctx: &CleaningContext) -> usize {
    let scan_limit = lines.len().min(80);
    let mut copyright_found = false;
    let mut end_idx: Option<usize> = None;

    for (i, line) in lines[..scan_limit].iter().enumerate() {
        if COPYRIGHT_RE.is_match(line) {
            copyright_found = true;
        }
        if copyright_found && SUPPORT_RE.is_match(line) {
            end_idx = Some(i);
            break;
        }
    }

    if let Some(idx) = end_idx {
        let removed = idx + 1;
        lines.drain(..removed);
        removed
    } else {
        0
    }
}

/// Rule 2: Remove page boundary blocks.
///
/// Pattern: `©` footer → blank lines → `Page N` → blank lines → doc title echo → section echo
fn rule_page_boundaries(lines: &mut Vec<String>, ctx: &CleaningContext) -> usize {
    // Find all Page N line indices
    let page_indices: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| PAGE_RE.is_match(line.trim()))
        .map(|(i, _)| i)
        .collect();

    if page_indices.is_empty() {
        return 0;
    }

    // For each Page N, determine the range to remove
    let mut ranges: Vec<(usize, usize)> = Vec::new();

    for &page_idx in &page_indices {
        // Look backward up to 5 lines for copyright footer
        let mut start_idx = page_idx;
        for i in (page_idx.saturating_sub(5)..page_idx).rev() {
            if PAGE_COPYRIGHT_RE.is_match(lines[i].trim()) {
                start_idx = i;
                break;
            }
        }

        // Look forward from Page N to find where real content starts
        let mut end_idx = page_idx + 1;
        let scan_end = lines.len().min(page_idx + 20);

        for (i, line) in lines[page_idx + 1..scan_end].iter().enumerate() {
            let i = i + page_idx + 1;
            let trimmed = line.trim();

            // Blank lines are part of the boundary
            if trimmed.is_empty() {
                end_idx = i + 1;
                continue;
            }

            // Headings signal real content
            if trimmed.starts_with('#') {
                break;
            }

            // Document title echo
            if !ctx.doc_title.is_empty()
                && (ctx.doc_title.contains(trimmed) || trimmed.contains(&ctx.doc_title))
            {
                end_idx = i + 1;
                continue;
            }

            // Section name echo: plain text, short, no special chars
            if trimmed.len() < 100
                && !trimmed.contains('•')
                && !trimmed.contains("**")
                && !trimmed.contains('`')
                && !trimmed.contains('[')
            {
                end_idx = i + 1;
                continue;
            }

            // Real content
            break;
        }

        ranges.push((start_idx, end_idx));
    }

    // Merge overlapping ranges
    ranges.sort_by_key(|r| r.0);
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (start, end) in ranges {
        if let Some(last) = merged.last_mut() {
            if start <= last.1 {
                last.1 = last.1.max(end);
                continue;
            }
        }
        merged.push((start, end));
    }

    // Remove ranges from bottom to top to preserve indices
    let mut removed = 0;
    for (start, end) in merged.into_iter().rev() {
        let count = end - start;
        lines.drain(start..end);
        removed += count;
    }

    removed
}

/// Rule 3: Remove table of contents section.
///
/// Finds `# Contents` and removes everything until the next H1 heading.
fn rule_toc_section(lines: &mut Vec<String>, _ctx: &CleaningContext) -> usize {
    let mut toc_start: Option<usize> = None;
    let mut toc_end: Option<usize> = None;

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();

        if trimmed == "# Contents" {
            toc_start = Some(i);
            continue;
        }

        if toc_start.is_some()
            && toc_end.is_none()
            && trimmed.starts_with("# ")
            && !trimmed.starts_with("## ")
        {
            toc_end = Some(i);
            break;
        }
    }

    if let Some(start) = toc_start {
        let end = toc_end.unwrap_or(lines.len());
        let removed = end - start;
        lines.drain(start..end);
        removed
    } else {
        0
    }
}

/// Rule 4: Strip bare `Chapter N` headings.
///
/// Removes lines matching `#{1,6} Chapter \d+` with nothing else.
fn rule_chapter_headings(lines: &mut Vec<String>, _ctx: &CleaningContext) -> usize {
    let before = lines.len();
    lines.retain(|line| !CHAPTER_RE.is_match(line.trim()));
    before - lines.len()
}

/// Rule 5: Replace `**•**` with `-`.
#[allow(clippy::ptr_arg)] // Signature matches CleaningRule.apply fn pointer type
fn rule_bold_bullets(lines: &mut Vec<String>, _ctx: &CleaningContext) -> usize {
    let mut replaced = 0;
    for line in lines.iter_mut() {
        if line.contains("**•**") {
            *line = line.replace("**•**", "-");
            replaced += 1;
        }
    }
    replaced
}

/// Rule 6: Remove `~~text~~` strikethrough markers, keeping the text.
#[allow(clippy::ptr_arg)] // Signature matches CleaningRule.apply fn pointer type
fn rule_strikethrough(lines: &mut Vec<String>, _ctx: &CleaningContext) -> usize {
    let mut replaced = 0;
    for line in lines.iter_mut() {
        let new = STRIKE_RE.replace_all(line, "$1").to_string();
        if new != *line {
            *line = new;
            replaced += 1;
        }
    }
    replaced
}

/// Rule 7: Collapse 3+ consecutive blank lines to exactly 2.
fn rule_blank_lines(lines: &mut Vec<String>, _ctx: &CleaningContext) -> usize {
    let mut result = Vec::with_capacity(lines.len());
    let mut blank_count = 0usize;
    let mut collapsed = 0usize;

    for line in lines.iter() {
        if line.trim().is_empty() {
            blank_count += 1;
        } else {
            if blank_count > 0 {
                let output_blanks = if blank_count >= 3 {
                    collapsed += blank_count - 2;
                    2
                } else {
                    blank_count
                };
                for _ in 0..output_blanks {
                    result.push(String::new());
                }
                blank_count = 0;
            }
            result.push(line.clone());
        }
    }

    // Handle trailing blanks
    if blank_count > 0 {
        let output_blanks = if blank_count >= 3 {
            collapsed += blank_count - 2;
            2
        } else {
            blank_count
        };
        for _ in 0..output_blanks {
            result.push(String::new());
        }
    }

    *lines = result;
    collapsed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> CleaningContext {
        CleaningContext {
            doc_title: String::new(),
        }
    }

    #[test]
    fn test_copyright_boilerplate() {
        let mut lines: Vec<String> = vec![
            "Some header".into(),
            "© 2015-2024 by AVEVA Group Limited".into(),
            "All rights reserved".into(),
            "softwaresupport.aveva.com".into(),
            "# Real Content".into(),
        ];
        let removed = rule_copyright_boilerplate(&mut lines, &ctx());
        assert_eq!(removed, 4);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], "# Real Content");
    }

    #[test]
    fn test_copyright_boilerplate_not_found() {
        let mut lines: Vec<String> = vec!["# Normal Document".into(), "Content".into()];
        let removed = rule_copyright_boilerplate(&mut lines, &ctx());
        assert_eq!(removed, 0);
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn test_toc_section() {
        let mut lines: Vec<String> = vec![
            "# Contents".into(),
            "- Item 1".into(),
            "- Item 2".into(),
            "# Introduction".into(),
            "Real content".into(),
        ];
        let removed = rule_toc_section(&mut lines, &ctx());
        assert_eq!(removed, 3);
        assert_eq!(lines[0], "# Introduction");
    }

    #[test]
    fn test_chapter_headings() {
        let mut lines: Vec<String> = vec![
            "# Overview".into(),
            "### Chapter 1".into(),
            "Content here".into(),
            "## Chapter 2".into(),
            "More content".into(),
        ];
        let removed = rule_chapter_headings(&mut lines, &ctx());
        assert_eq!(removed, 2);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "# Overview");
        assert_eq!(lines[1], "Content here");
        assert_eq!(lines[2], "More content");
    }

    #[test]
    fn test_bold_bullets() {
        let mut lines: Vec<String> = vec!["**•** First item".into(), "Normal line".into()];
        let replaced = rule_bold_bullets(&mut lines, &ctx());
        assert_eq!(replaced, 1);
        assert_eq!(lines[0], "- First item");
    }

    #[test]
    fn test_strikethrough() {
        let mut lines: Vec<String> = vec!["This is ~~deleted~~ text".into()];
        let replaced = rule_strikethrough(&mut lines, &ctx());
        assert_eq!(replaced, 1);
        assert_eq!(lines[0], "This is deleted text");
    }

    #[test]
    fn test_blank_lines_collapse() {
        let mut lines: Vec<String> = vec![
            "Line 1".into(),
            "".into(),
            "".into(),
            "".into(),
            "".into(),
            "Line 2".into(),
        ];
        let collapsed = rule_blank_lines(&mut lines, &ctx());
        assert_eq!(collapsed, 2);
        assert_eq!(lines.len(), 4); // Line 1, blank, blank, Line 2
        assert_eq!(lines[0], "Line 1");
        assert_eq!(lines[1], "");
        assert_eq!(lines[2], "");
        assert_eq!(lines[3], "Line 2");
    }

    #[test]
    fn test_blank_lines_no_collapse() {
        let mut lines: Vec<String> = vec!["Line 1".into(), "".into(), "".into(), "Line 2".into()];
        let collapsed = rule_blank_lines(&mut lines, &ctx());
        assert_eq!(collapsed, 0);
        assert_eq!(lines.len(), 4);
    }

    #[test]
    fn test_clean_markdown_all_tags() {
        let input = "# Contents\n- TOC\n# Real Title\nContent\n\n\n\n\nMore content\n";
        let result = clean_markdown(input, &[]);
        assert!(!result.contains("# Contents"));
        assert!(result.contains("# Real Title"));
        // Blank lines collapsed
        assert!(!result.contains("\n\n\n\n"));
    }

    #[test]
    fn test_clean_markdown_tag_filter() {
        let input =
            "© 2015-2024 by AVEVA Group Limited\nAll rights\nsoftwaresupport.aveva.com\n# Title\n";
        // Only run generic rules, skip aveva-specific
        let result = clean_markdown(input, &["generic"]);
        // Copyright boilerplate should still be present (aveva tag not requested)
        assert!(result.contains("AVEVA"));
    }

    #[test]
    fn test_page_boundaries() {
        let mut lines: Vec<String> = vec![
            "Content before".into(),
            "© AVEVA 2024".into(),
            "".into(),
            "Page 1".into(),
            "".into(),
            "# Next Section".into(),
        ];
        let ctx = CleaningContext {
            doc_title: "My Doc".to_string(),
        };
        let removed = rule_page_boundaries(&mut lines, &ctx);
        assert!(removed > 0);
        // The heading should survive
        assert!(lines.iter().any(|l| l.contains("# Next Section")));
    }
}
