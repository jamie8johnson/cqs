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
    pub file: PathBuf,
    pub line: u32,
    pub call_line: u32,
    pub snippet: Option<String>,
}

/// Affected test with call depth
#[derive(Debug, Clone, serde::Serialize)]
pub struct TestInfo {
    pub name: String,
    pub file: PathBuf,
    pub line: u32,
    pub call_depth: usize,
}

/// Transitive caller at a given depth
#[derive(Debug, Clone, serde::Serialize)]
pub struct TransitiveCaller {
    pub name: String,
    pub file: PathBuf,
    pub line: u32,
    pub depth: usize,
}

/// Complete impact analysis result
#[derive(Debug, Clone, serde::Serialize)]
pub struct ImpactResult {
    pub function_name: String,
    pub callers: Vec<CallerDetail>,
    pub tests: Vec<TestInfo>,
    pub transitive_callers: Vec<TransitiveCaller>,
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
    pub file: String,
    pub line_start: u32,
}

/// A test affected by diff changes, tracking which changed function leads to it
#[derive(Debug, Clone, serde::Serialize)]
pub struct DiffTestInfo {
    pub name: String,
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
    pub suggested_file: String,
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
    pub coverage: f32,
    pub risk_level: RiskLevel,
    /// Blast radius based on caller count alone (Low 0-2, Medium 3-10, High >10).
    /// Unlike `risk_level`, this does NOT decrease with test coverage.
    pub blast_radius: RiskLevel,
    pub score: f32,
}
