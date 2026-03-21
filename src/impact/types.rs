//! Data types for impact analysis

use std::path::PathBuf;

/// Direct caller with display-ready fields (call-site context + snippet).
///
/// Named `CallerDetail` to distinguish from `store::CallerInfo` which has
/// only basic fields (name, file, line). This struct adds `call_line` and
/// `snippet` for impact analysis display.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CallerDetail {
    pub name: String,
    #[serde(serialize_with = "crate::serialize_path_normalized")]
    pub file: PathBuf,
    pub line: u32,
    pub call_line: u32,
    pub snippet: Option<String>,
}

/// Affected test with call depth
#[derive(Debug, Clone, serde::Serialize)]
pub struct TestInfo {
    pub name: String,
    #[serde(serialize_with = "crate::serialize_path_normalized")]
    pub file: PathBuf,
    pub line: u32,
    pub call_depth: usize,
}

impl TestInfo {
    /// Serialize to JSON, relativizing file paths against the project root.
    pub fn to_json(&self, root: &std::path::Path) -> serde_json::Value {
        serde_json::json!({
            "name": self.name,
            "file": crate::rel_display(&self.file, root),
            "line": self.line,
            "call_depth": self.call_depth,
        })
    }
}

/// Transitive caller at a given depth
#[derive(Debug, Clone, serde::Serialize)]
pub struct TransitiveCaller {
    pub name: String,
    #[serde(serialize_with = "crate::serialize_path_normalized")]
    pub file: PathBuf,
    pub line: u32,
    pub depth: usize,
}

/// A function impacted via shared type dependencies (one-hop type expansion).
#[derive(Debug, Clone, serde::Serialize)]
pub struct TypeImpacted {
    pub name: String,
    #[serde(serialize_with = "crate::serialize_path_normalized")]
    pub file: PathBuf,
    pub line: u32,
    pub shared_types: Vec<String>,
}

/// Complete impact analysis result
#[derive(Debug, Clone, serde::Serialize)]
pub struct ImpactResult {
    pub function_name: String,
    pub callers: Vec<CallerDetail>,
    pub tests: Vec<TestInfo>,
    pub transitive_callers: Vec<TransitiveCaller>,
    pub type_impacted: Vec<TypeImpacted>,
    /// True when batch name search failed and caller snippets may be incomplete.
    /// CLI handlers can display a warning when this is set.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub degraded: bool,
}

/// Lightweight caller + test coverage hints for a function.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FunctionHints {
    pub caller_count: usize,
    pub test_count: usize,
}

/// A function identified as changed by a diff
#[derive(Debug, Clone, serde::Serialize)]
pub struct ChangedFunction {
    pub name: String,
    #[serde(serialize_with = "crate::serialize_path_normalized")]
    pub file: PathBuf,
    pub line_start: u32,
}

/// A test affected by diff changes, tracking which changed function leads to it
#[derive(Debug, Clone, serde::Serialize)]
pub struct DiffTestInfo {
    pub name: String,
    #[serde(serialize_with = "crate::serialize_path_normalized")]
    pub file: PathBuf,
    pub line: u32,
    pub via: String,
    pub call_depth: usize,
}

/// Summary counts for diff impact
#[derive(Debug, Clone, serde::Serialize)]
pub struct DiffImpactSummary {
    pub changed_count: usize,
    pub caller_count: usize,
    pub test_count: usize,
}

/// Aggregated impact result from a diff
#[derive(Debug, Clone, serde::Serialize)]
pub struct DiffImpactResult {
    pub changed_functions: Vec<ChangedFunction>,
    pub all_callers: Vec<CallerDetail>,
    pub all_tests: Vec<DiffTestInfo>,
    pub summary: DiffImpactSummary,
}

/// A suggested test for an untested caller
#[derive(Debug, Clone, serde::Serialize)]
pub struct TestSuggestion {
    /// Suggested test function name
    pub test_name: String,
    /// Suggested file for the test
    #[serde(serialize_with = "crate::serialize_path_normalized")]
    pub suggested_file: PathBuf,
    /// The untested function this test would cover
    pub for_function: String,
    /// Where the naming pattern came from (empty if default)
    pub pattern_source: String,
    /// Whether to put the test inline (vs external test file)
    pub inline: bool,
}

/// Risk level for a function based on caller count and test coverage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RiskLevel {
    High,
    Medium,
    Low,
}

impl std::fmt::Display for RiskLevel {
    /// Formats the RiskLevel enum variant as a human-readable string.
    ///
    /// # Arguments
    ///
    /// * `f` - The formatter to write the output to.
    ///
    /// # Returns
    ///
    /// A `std::fmt::Result` indicating whether the formatting operation succeeded.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RiskLevel::High => write!(f, "high"),
            RiskLevel::Medium => write!(f, "medium"),
            RiskLevel::Low => write!(f, "low"),
        }
    }
}

/// Risk assessment for a single function.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RiskScore {
    pub caller_count: usize,
    pub test_count: usize,
    /// Ratio of test_count to caller_count, capped at 1.0.
    ///
    /// This is NOT transitive test coverage -- it is `min(test_count / max(caller_count, 1), 1.0)`.
    /// A value of 1.0 means at least as many tests reach this function as callers exist,
    /// but does not guarantee every caller path is tested.
    pub test_ratio: f32,
    pub risk_level: RiskLevel,
    /// Blast radius based on caller count alone (Low 0-2, Medium 3-10, High >10).
    /// Unlike `risk_level`, this does NOT decrease with test coverage.
    pub blast_radius: RiskLevel,
    pub score: f32,
}

impl RiskScore {
    /// Serialize to JSON with the associated function name.
    pub fn to_json(&self, name: &str) -> serde_json::Value {
        serde_json::json!({
            "name": name,
            "risk_level": self.risk_level.to_string(),
            "blast_radius": self.blast_radius.to_string(),
            "score": self.score,
            "caller_count": self.caller_count,
            "test_count": self.test_count,
            "test_ratio": self.test_ratio,
        })
    }
}
