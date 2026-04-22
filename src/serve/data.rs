//! Wire-format types for `/api/*` endpoints.
//!
//! Frontend Cytoscape.js consumes the `Node` + `Edge` shapes directly.
//! Field names match Cytoscape's element-data convention so the JS can
//! pass the rows through without transformation.

use serde::{Deserialize, Serialize};

/// One node in the call graph. Cytoscape renders one of these per
/// chunk (or per windowed-chunk row, since each window has its own
/// embedding + identity).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Node {
    /// Stable chunk ID — used as Cytoscape `data.id`.
    pub id: String,
    /// Display name (function name, struct name, etc.).
    pub name: String,
    /// Chunk type (function, method, struct, impl, ...). Drives
    /// node color via CSS class.
    #[serde(rename = "type")]
    pub kind: String,
    /// Source language (`rust`, `python`, ...). Drives optional
    /// per-language CSS rules.
    pub language: String,
    /// File path relative to project root.
    pub file: String,
    pub line_start: u32,
    pub line_end: u32,
    /// Number of incoming call edges. Drives node size (sqrt scaling).
    pub n_callers: u32,
    /// Number of outgoing call edges.
    pub n_callees: u32,
    /// True if the chunk has zero callers and zero tests covering it
    /// — flagged with a red ring + opacity drop in the UI.
    pub dead: bool,
}

/// One edge in the call graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Edge {
    /// Source chunk ID.
    pub source: String,
    /// Target chunk ID.
    pub target: String,
    /// `call` for callee edges, `type_dep` for type dependencies.
    /// v1 only emits `call`; `type_dep` is wired here for future
    /// view-mode toggles.
    pub kind: String,
}

/// Top-level response for `GET /api/graph`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct GraphResponse {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

/// Detail payload for the click-sidebar (`GET /api/chunk/:id`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ChunkDetail {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub language: String,
    pub file: String,
    pub line_start: u32,
    pub line_end: u32,
    pub signature: String,
    pub doc: Option<String>,
    /// First N lines of the chunk content for inline preview.
    pub content_preview: String,
    pub callers: Vec<NodeRef>,
    pub callees: Vec<NodeRef>,
    pub tests: Vec<NodeRef>,
}

/// Compact reference used in caller/callee/tests lists. Just enough
/// to render a clickable link in the sidebar.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct NodeRef {
    pub id: String,
    pub name: String,
    pub file: String,
    pub line_start: u32,
}

/// Response for `GET /api/search?q=...`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SearchResponse {
    pub matches: Vec<NodeRef>,
}

/// Response for `GET /api/stats`. Mirrors a small subset of `cqs stats`
/// for the header bar.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct StatsResponse {
    pub total_chunks: u64,
    pub total_files: u64,
    pub call_edges: u64,
    pub type_edges: u64,
}
