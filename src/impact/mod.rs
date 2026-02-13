//! Impact analysis core
//!
//! Provides BFS caller traversal, test discovery, snippet extraction,
//! transitive caller analysis, and mermaid diagram generation.

mod analysis;
mod bfs;
mod diff;
mod format;
mod hints;
mod types;

// Re-export types used by lib.rs and other crate modules
pub use types::{
    CallerDetail, ChangedFunction, DiffImpactResult, DiffImpactSummary, DiffTestInfo,
    FunctionHints, ImpactResult, RiskLevel, RiskScore, TestInfo, TestSuggestion, TransitiveCaller,
};

// Re-export public functions
pub use analysis::{analyze_impact, suggest_tests};
pub use diff::{analyze_diff_impact, analyze_diff_impact_with_graph, map_hunks_to_functions};
pub use format::{diff_impact_to_json, impact_to_json, impact_to_mermaid};
pub use hints::{
    compute_hints, compute_hints_with_graph, compute_hints_with_graph_depth, compute_risk_batch,
    find_hotspots,
};

/// Default maximum depth for test search BFS.
/// Exposed via `max_test_depth` parameters on analysis functions.
pub const DEFAULT_MAX_TEST_SEARCH_DEPTH: usize = 5;
