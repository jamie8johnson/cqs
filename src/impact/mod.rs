//! Impact analysis core
//!
//! Provides BFS caller traversal, test discovery, snippet extraction,
//! transitive caller analysis, and mermaid diagram generation.

mod analysis;
mod bfs;
pub mod cross_project;
mod diff;
mod format;
mod hints;
mod test_map;
mod types;

// Re-export types used by lib.rs and other crate modules
pub use types::{
    CallerDetail, ChangedFunction, DiffImpactResult, DiffImpactSummary, DiffTestInfo,
    FunctionHints, ImpactResult, RiskLevel, RiskScore, TestInfo, TestSuggestion, TransitiveCaller,
    TypeImpacted,
};

// Re-export public functions
pub(crate) use analysis::find_affected_tests_with_chunks;
pub use analysis::{analyze_impact, suggest_tests, ImpactOptions};
pub use diff::{analyze_diff_impact, analyze_diff_impact_with_graph, map_hunks_to_functions};
pub use format::{diff_impact_to_json, format_test_suggestions, impact_to_json, impact_to_mermaid};
pub use hints::{
    compute_hints, compute_hints_batch, compute_hints_with_graph, compute_risk_and_tests,
    compute_risk_batch, find_hotspots,
};
pub use test_map::{find_test_matches, TestMatch};

/// Default maximum depth for test search BFS.
/// Exposed via `max_test_depth` parameters on analysis functions.
pub const DEFAULT_MAX_TEST_SEARCH_DEPTH: usize = 5;
