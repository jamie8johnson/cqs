//! Data types for impact analysis

use std::path::PathBuf;

/// Direct caller with display-ready fields (call-site context + snippet).
/// Named `CallerDetail` to distinguish from `store::CallerInfo` which has
/// only basic fields (name, file, line). This struct adds `call_line` and
/// `snippet` for impact analysis display.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CallerDetail {
    pub name: String,
    #[serde(serialize_with = "crate::serialize_path_normalized")]
    pub file: PathBuf,
    #[serde(rename = "line_start")]
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
    #[serde(rename = "line_start")]
    pub line: u32,
    pub call_depth: usize,
}

/// Transitive caller at a given depth
#[derive(Debug, Clone, serde::Serialize)]
pub struct TransitiveCaller {
    pub name: String,
    #[serde(serialize_with = "crate::serialize_path_normalized")]
    pub file: PathBuf,
    #[serde(rename = "line_start")]
    pub line: u32,
    pub depth: usize,
}

/// A function impacted via shared type dependencies (one-hop type expansion).
#[derive(Debug, Clone, serde::Serialize)]
pub struct TypeImpacted {
    pub name: String,
    #[serde(serialize_with = "crate::serialize_path_normalized")]
    pub file: PathBuf,
    #[serde(rename = "line_start")]
    pub line: u32,
    pub shared_types: Vec<String>,
}

/// Complete impact analysis result.
///
/// Serializes with computed `caller_count`, `test_count`, and `type_impacted_count`
/// fields derived from vec lengths, so callers get counts natively without
/// post-serialization mutation.
#[derive(Debug, Clone)]
pub struct ImpactResult {
    pub function_name: String,
    pub callers: Vec<CallerDetail>,
    pub tests: Vec<TestInfo>,
    pub transitive_callers: Vec<TransitiveCaller>,
    pub type_impacted: Vec<TypeImpacted>,
    /// True when batch name search failed and caller snippets may be incomplete.
    /// CLI handlers can display a warning when this is set.
    pub degraded: bool,
}

impl serde::Serialize for ImpactResult {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;

        // Count non-empty optional fields to determine struct length
        let mut field_count = 6; // name, callers, caller_count, tests, test_count, type_impacted, type_impacted_count
        field_count += 1; // type_impacted_count
        if !self.transitive_callers.is_empty() {
            field_count += 1;
        }
        if self.degraded {
            field_count += 1;
        }

        let mut state = serializer.serialize_struct("ImpactResult", field_count)?;
        state.serialize_field("name", &self.function_name)?;
        state.serialize_field("callers", &self.callers)?;
        state.serialize_field("caller_count", &self.callers.len())?;
        state.serialize_field("tests", &self.tests)?;
        state.serialize_field("test_count", &self.tests.len())?;
        if !self.transitive_callers.is_empty() {
            state.serialize_field("transitive_callers", &self.transitive_callers)?;
        }
        state.serialize_field("type_impacted", &self.type_impacted)?;
        state.serialize_field("type_impacted_count", &self.type_impacted.len())?;
        if self.degraded {
            state.serialize_field("degraded", &true)?;
        }
        state.end()
    }
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
    #[serde(rename = "line_start")]
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
    /// True when changed functions exceeded the cap and were truncated.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    /// SHL-V1.25-7: number of changed functions dropped past the cap. `--json`
    /// consumers can detect silent truncation without scraping stderr. Zero
    /// when `truncated == false`.
    #[serde(skip_serializing_if = "is_zero_usize")]
    pub truncated_functions: usize,
}

fn is_zero_usize(n: &usize) -> bool {
    *n == 0
}

/// Aggregated impact result from a diff
#[derive(Debug, Clone, serde::Serialize)]
pub struct DiffImpactResult {
    pub changed_functions: Vec<ChangedFunction>,
    #[serde(rename = "callers")]
    pub all_callers: Vec<CallerDetail>,
    #[serde(rename = "tests")]
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
    /// # Arguments
    /// * `f` - The formatter to write the output to.
    /// # Returns
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
