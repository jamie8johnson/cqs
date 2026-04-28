//! Typed output structs for batch JSON responses.
//!
//! Replaces manual `serde_json::json!` assembly with `#[derive(Serialize)]` structs
//! for chunk-shaped output. Ensures consistent field names and path normalization.

use serde::Serialize;

use cqs::normalize_path;

/// Common chunk output shape used by search, similar, and other handlers.
///
/// Includes `trust_level` (#1167, #1169) to give consuming agents an explicit
/// signal that distinguishes the user's own code from third-party reference
/// content. `reference_name` is set on results from `cqs ref` indexes so an
/// agent can map a chunk back to its originating reference without re-querying.
/// `injection_flags` (#1181) lists every injection-pattern heuristic that
/// fired on the chunk's raw content — empty when nothing matched, always
/// present so the schema stays stable.
#[derive(Serialize)]
pub(super) struct ChunkOutput {
    pub name: String,
    pub file: String,
    pub line_start: u32,
    pub line_end: u32,
    pub language: String,
    pub chunk_type: String,
    pub score: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// "user-code" for chunks from the user's own project, "reference-code"
    /// for chunks from a `cqs ref` index. Always present.
    pub trust_level: &'static str,
    /// Name of the originating reference; only present when the chunk came
    /// from a `cqs ref` index.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference_name: Option<String>,
    /// Injection-pattern heuristics that fired on the chunk's raw content
    /// (e.g. `["leading-directive", "code-fence"]`). Empty `Vec` when
    /// nothing matched. Always present so consumers can rely on the field.
    pub injection_flags: Vec<&'static str>,
}

impl ChunkOutput {
    /// Build from a SearchResult (search, name-only search).
    ///
    /// Equivalent to `from_search_result_with_origin(r, include_content, None)`:
    /// emits `trust_level: "user-code"` and omits `reference_name`.
    pub fn from_search_result(r: &cqs::store::SearchResult, include_content: bool) -> Self {
        Self::from_search_result_with_origin(r, include_content, None)
    }

    /// Build from a SearchResult, tagging the trust origin.
    ///
    /// `ref_name = None` ⇒ `trust_level: "user-code"`. `ref_name = Some(name)`
    /// ⇒ `trust_level: "reference-code"` plus `reference_name: <name>`.
    pub fn from_search_result_with_origin(
        r: &cqs::store::SearchResult,
        include_content: bool,
        ref_name: Option<&str>,
    ) -> Self {
        let trust_level = if ref_name.is_some() {
            "reference-code"
        } else {
            "user-code"
        };
        Self {
            name: r.chunk.name.clone(),
            file: normalize_path(&r.chunk.file),
            line_start: r.chunk.line_start,
            line_end: r.chunk.line_end,
            language: r.chunk.language.to_string(),
            chunk_type: r.chunk.chunk_type.to_string(),
            score: r.score,
            signature: Some(r.chunk.signature.clone()),
            content: if include_content {
                Some(maybe_wrap_content(&r.chunk.content, &r.chunk.id))
            } else {
                None
            },
            trust_level,
            reference_name: ref_name.map(str::to_string),
            injection_flags: cqs::llm::validation::detect_all_injection_patterns(&r.chunk.content),
        }
    }
}

/// Wrap chunk content in trust-boundary delimiters unless `CQS_TRUST_DELIMITERS=0`.
///
/// Default-on since #1181 — the wrapping is the visible boundary that
/// downstream injection guards key off when the chunk is inlined into a
/// larger prompt. Set `CQS_TRUST_DELIMITERS=0` to opt out (raw text).
fn maybe_wrap_content(content: &str, id: &str) -> String {
    if std::env::var("CQS_TRUST_DELIMITERS").as_deref() == Ok("0") {
        content.to_string()
    } else {
        format!("<<<chunk:{id}>>>\n{content}\n<<</chunk:{id}>>>")
    }
}
