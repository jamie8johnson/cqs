//! Shared deserialization types for the v3 eval query format.
//!
//! Used by the production runner (`cqs eval`) and integration tests.
//! Previously these lived inline in `src/cli/commands/eval/runner.rs` —
//! moving them here closes the audit P2 #61 finding (three independent
//! sources of truth for "what does an eval row look like").
//!
//! Wire format: `evals/queries/v3_*.json`. The on-disk envelope carries
//! `judges`, `metadata`, and other fields that the runner ignores; we
//! keep parsing forgiving (no `deny_unknown_fields`) so a future eval set
//! with a different envelope still loads. The `#[test]` in
//! `tests/eval_test.rs` (audit #61c) explicitly opts in to
//! `deny_unknown_fields` to surface field drops at test time.
//!
//! Adding a field that the runner needs to consume:
//!   1. Add it here (with serde defaults if optional).
//!   2. Update consumers in `runner.rs` and any test helpers that read it.
//!   3. Bump the round-trip test in `tests/eval_test.rs` to assert the
//!      new field's deserialization invariants.

use serde::{Deserialize, Serialize};

/// Top-level container for an eval query set as serialized to JSON.
///
/// The `queries` array is the only field the runner reads; envelope
/// metadata (`schema_version`, `n`, `category_counts`, `tier_counts`,
/// `split`, `created_at`) is intentionally not modeled here so a future
/// eval generator can extend the envelope without breaking the runner.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuerySet {
    pub queries: Vec<EvalQuery>,
}

/// One eval query with its expected gold chunk.
///
/// Gold matching uses `(file == origin) AND (name == name) AND
/// (line_start == line_start)` — the v3 standard. Queries without a gold
/// chunk are skipped at scoring time (counted in `skipped`, not in
/// `total`) so reported R@K is over scoreable queries only.
///
/// Auxiliary fields from the on-disk format (`judges`, `metadata`,
/// `pool_size`, `tier`, `gold_chunk_source`, `source`, `tags`) are
/// declared so they don't get silently dropped under `deny_unknown_fields`
/// in tests; the runner ignores them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalQuery {
    pub query: String,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub gold_chunk: Option<GoldChunk>,
    /// Optional grouping for partitioned eval reports. Common values:
    /// `"telemetry"`, `"generated"`, `"consensus"`. The runner ignores
    /// it but it's modeled so `deny_unknown_fields` doesn't trip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Per-judge verdicts — opaque object kept for forward-compat. The
    /// runner doesn't read this; tests assert it survives a round-trip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judges: Option<serde_json::Value>,
    /// Free-form metadata blob (e.g., `{first_seen_ts, source_cmd,
    /// target_category, matched}`). Treated as opaque.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    /// Pool size used during gold consensus (number of distinct chunks
    /// returned across the configurations the v3 pipeline consulted).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool_size: Option<u32>,
    /// Confidence tier (e.g., `"high_confidence"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
    /// Provenance for the chosen `gold_chunk` (e.g., `"consensus"`,
    /// `"override"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gold_chunk_source: Option<String>,
    /// Optional secondary tags carried by some generators. Unused by the
    /// runner; declared so `deny_unknown_fields` accepts them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Marker set by the v3 generator when the gold chunk could not be
    /// resolved against the live index. The runner treats these as misses;
    /// modeled so `deny_unknown_fields` parsers don't reject them.
    #[serde(default, rename = "_unresolved", skip_serializing_if = "is_false")]
    pub unresolved: bool,
}

/// `serde(skip_serializing_if)` requires a `fn(&T) -> bool`; closures and
/// trait methods don't work here. Used by `EvalQuery::unresolved`.
fn is_false(b: &bool) -> bool {
    !*b
}

/// The expected matching chunk. `origin` is the file path as indexed.
///
/// `id`, `line_end`, `chunk_type`, `language` are present in the v3 file
/// format but not used for matching; declared so `deny_unknown_fields`
/// doesn't trip in the round-trip test.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoldChunk {
    pub name: String,
    pub origin: String,
    pub line_start: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_end: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunk_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sanity: the minimal v3-shaped payload deserializes. This mirrors
    /// the equivalent assertion the production runner used to carry; we
    /// keep it here so the schema's contract is testable in isolation.
    #[test]
    fn test_query_set_parses_v3_envelope() {
        let raw = r#"{
            "schema_version": "v3-consensus",
            "n": 1,
            "queries": [
                {
                    "query": "table named notes",
                    "category": "multi_step",
                    "metadata": {"target_category": "multi_step"},
                    "judges": {"claude": {"verified": true}},
                    "gold_chunk": {
                        "id": "src/schema.sql:108:744fc0db",
                        "name": "notes",
                        "origin": "src/schema.sql",
                        "language": "sql",
                        "chunk_type": "struct",
                        "line_start": 108,
                        "line_end": 118
                    }
                }
            ]
        }"#;

        let set: QuerySet = serde_json::from_str(raw).expect("v3 envelope must parse");
        assert_eq!(set.queries.len(), 1);
        let q = &set.queries[0];
        assert_eq!(q.query, "table named notes");
        assert_eq!(q.category.as_deref(), Some("multi_step"));
        let gold = q.gold_chunk.as_ref().expect("gold_chunk must parse");
        assert_eq!(gold.name, "notes");
        assert_eq!(gold.origin, "src/schema.sql");
        assert_eq!(gold.line_start, 108);
    }

    /// Forward compat: a query with no gold_chunk and no category still
    /// parses (will be reported as `skipped` at runtime).
    #[test]
    fn test_query_set_parses_minimal() {
        let raw = r#"{"queries": [{"query": "anything"}]}"#;
        let set: QuerySet = serde_json::from_str(raw).expect("minimal payload must parse");
        assert_eq!(set.queries.len(), 1);
        assert!(set.queries[0].gold_chunk.is_none());
        assert!(set.queries[0].category.is_none());
    }
}
