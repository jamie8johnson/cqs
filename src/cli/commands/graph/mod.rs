//! Graph commands — call graph analysis, impact, tracing, type dependencies

mod callers;
mod deps;
pub(crate) mod explain;
mod impact;
mod impact_diff;
mod test_map;
pub(crate) mod trace;

pub(crate) use callers::{build_callees, build_callers, cmd_callees, cmd_callers};
pub(crate) use deps::{build_deps_forward, build_deps_reverse, cmd_deps};
pub(crate) use explain::cmd_explain;
pub(crate) use impact::cmd_impact;
pub(crate) use impact_diff::cmd_impact_diff;
pub(crate) use test_map::{build_test_map, build_test_map_output, cmd_test_map};
pub(crate) use trace::cmd_trace;

/// Maximum number of `definitions[]` entries returned in a kind-mismatch
/// fallback response. Mirrors the standard graph-command result cap.
///
/// Shared by every CLI graph command (callers, callees, deps, impact,
/// test-map, trace) and the daemon dispatch handler so a hot name like
/// `Result` / `Error` matching hundreds of chunks can never balloon a
/// fallback response into multi-MB JSON.
pub(crate) const KIND_FALLBACK_MAX_DEFINITIONS: usize = 100;

/// Per-entry `content` byte cap inside a kind-mismatch fallback
/// `definitions[]` entry. Truncated content is suffixed with
/// `"... (truncated)"` and the entry gains a `truncated: true` field
/// so consumers can distinguish capped chunks from full ones.
pub(crate) const KIND_FALLBACK_MAX_CONTENT_BYTES: usize = 2048;

/// Shared chunk-to-definition transformation for every CLI-direct kind
/// fallback (callers, callees, deps, impact, test-map, trace) and the
/// daemon dispatch path. Each entry carries
/// file/line_start/line_end/language/chunk_type/signature/content — the
/// same shape every kind emits — and truncates content per
/// [`KIND_FALLBACK_MAX_CONTENT_BYTES`].
pub(crate) fn chunk_to_definition_value(c: &cqs::store::ChunkSummary) -> serde_json::Value {
    let _span = tracing::trace_span!("chunk_to_definition_value").entered();
    let (content, truncated) = if c.content.len() > KIND_FALLBACK_MAX_CONTENT_BYTES {
        // Truncate at a UTF-8 char boundary at or below the byte cap.
        // `floor_char_boundary` would be cleaner but isn't stable yet.
        let mut end = KIND_FALLBACK_MAX_CONTENT_BYTES;
        while !c.content.is_char_boundary(end) {
            end -= 1;
        }
        (format!("{}... (truncated)", &c.content[..end]), true)
    } else {
        (c.content.clone(), false)
    };
    let mut entry = serde_json::Map::new();
    entry.insert(
        "file".to_string(),
        serde_json::json!(cqs::normalize_path(&c.file)),
    );
    entry.insert("line_start".to_string(), serde_json::json!(c.line_start));
    entry.insert("line_end".to_string(), serde_json::json!(c.line_end));
    entry.insert(
        "language".to_string(),
        serde_json::json!(c.language.to_string()),
    );
    entry.insert(
        "chunk_type".to_string(),
        serde_json::json!(c.chunk_type.to_string()),
    );
    entry.insert("signature".to_string(), serde_json::json!(c.signature));
    entry.insert("content".to_string(), serde_json::json!(content));
    if truncated {
        entry.insert("truncated".to_string(), serde_json::json!(true));
    }
    serde_json::Value::Object(entry)
}

/// Build a capped `definitions[]` list from chunks for kind-mismatch
/// fallbacks. Takes at most [`KIND_FALLBACK_MAX_DEFINITIONS`] entries and
/// truncates each chunk's content via [`chunk_to_definition_value`]. Used
/// by every CLI graph command's kind fallback so the count + content caps
/// hold uniformly across all surfaces.
pub(crate) fn chunks_to_definitions(chunks: &[cqs::store::ChunkSummary]) -> Vec<serde_json::Value> {
    chunks
        .iter()
        .take(KIND_FALLBACK_MAX_DEFINITIONS)
        .map(chunk_to_definition_value)
        .collect()
}
