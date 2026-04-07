//! Eval schema: shared types consumed by both query set generation and the eval harness.
//!
//! The query JSON file is deserialized into these types. Any change here requires
//! re-encoding the query set — so get it right before generating 300 queries.

use serde::{Deserialize, Serialize};

/// A single eval query with ground truth and categorization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalQuery {
    /// Unique identifier (e.g., "beh-042", "id-007")
    pub id: String,

    /// The natural-language query text as a user would type it
    pub query: String,

    /// Primary category (exactly one)
    pub category: QueryCategory,

    /// Secondary tags (zero or more)
    #[serde(default)]
    pub tags: Vec<QueryTag>,

    /// Language filter to apply during retrieval (None = all languages)
    pub language: Option<String>,

    /// The single chunk that is the correct top-1 result
    pub primary_answer: GroundTruth,

    /// Additional chunks that are also valid answers (up to 5)
    #[serde(default)]
    pub acceptable_answers: Vec<GroundTruth>,

    /// Chunks that look similar but are wrong — for hard negative analysis
    #[serde(default)]
    pub negative_examples: Vec<GroundTruth>,

    /// Which split this query belongs to
    pub split: Split,
}

/// Ground truth: identifies a chunk in the index
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroundTruth {
    /// Chunk name as it appears in the index (e.g., "search_filtered")
    pub name: String,

    /// File path relative to project root (e.g., "src/search/mod.rs")
    pub file: String,

    /// Optional: specific line range for disambiguation when name is ambiguous
    #[serde(default)]
    pub line_start: Option<u32>,
}

/// Primary query category — mutually exclusive
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryCategory {
    /// "find the parse_chunk function" — literal name or near-literal
    IdentifierLookup,
    /// "function that retries on network failure" — describes behavior
    BehavioralSearch,
    /// "find dead code" — domain vocabulary, may not appear in code
    ConceptualSearch,
    /// "tests for the embedding pipeline" — implies chunk type filter
    TypeFiltered,
    /// "json parsing" across multiple languages
    CrossLanguage,
    /// "implementations of the Searchable trait" — code structure
    StructuralSearch,
    /// "parse function that's NOT for CLI args" — constraint/disambiguation
    Negation,
    /// "how does the index get populated" — multi-chunk traversal
    MultiStep,
}

/// Secondary tags — zero or more per query
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryTag {
    /// Answer requires understanding code split across multiple files
    CrossFile,
    /// Answer is in a chunk added in the last 30 days
    RecentAdd,
    /// Codebase has multiple plausibly-similar wrong answers
    NoiseTolerant,
    /// Query uses words that don't literally appear in the answer
    SynonymHeavy,
    /// Query uses or expects acronyms (HNSW, RRF, etc.)
    Acronym,
    /// Answer depends on case distinction (Box vs box)
    CaseSensitive,
}

/// Train/held-out split
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Split {
    Train,
    HeldOut,
}

/// Full query set with metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalQuerySet {
    /// Version identifier (e.g., "v2_300q")
    pub version: String,
    /// When this query set was generated
    pub created: String,
    /// Description
    pub description: String,
    /// The queries
    pub queries: Vec<EvalQuery>,
}

/// Per-query result from a single eval run
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryResult {
    /// Run identifier
    pub run_id: String,
    /// Query identifier (matches EvalQuery.id)
    pub query_id: String,
    /// Configuration identifier (e.g., "bge-large_splade-none_rerank-none")
    pub config_id: String,
    /// Rank of the primary answer (None if not in top-100)
    pub rank_of_correct: Option<u32>,
    /// 1/rank (0 if not found)
    pub reciprocal_rank: f64,
    /// Primary answer was rank 1
    pub top_1_correct: bool,
    /// Primary answer was in top 5
    pub top_5_correct: bool,
    /// Any acceptable answer was in top 5
    pub top_5_acceptable: bool,
    /// Score of the rank-1 result
    pub top_1_score: f64,
    /// Score of the rank-2 result (for confidence gap)
    pub top_2_score: f64,
    /// Retrieval time in milliseconds
    pub retrieval_ms: f64,
    /// Rerank time in milliseconds (0 if no reranker)
    pub rerank_ms: f64,
}

/// Aggregate metrics with bootstrap confidence intervals
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AggregateMetrics {
    pub config_id: String,
    pub n: usize,
    pub r_at_1: MetricWithCI,
    pub r_at_5: MetricWithCI,
    pub r_at_5_acceptable: MetricWithCI,
    pub mrr: MetricWithCI,
    /// Optional per-category breakdown
    #[serde(default)]
    pub per_category: Vec<CategoryMetrics>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricWithCI {
    pub value: f64,
    pub ci_lower: f64,
    pub ci_upper: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CategoryMetrics {
    pub category: QueryCategory,
    pub n: usize,
    pub r_at_1: MetricWithCI,
    pub mrr: MetricWithCI,
}
