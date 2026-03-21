//! Scoring algorithms, name matching, and search helpers.
//!
//! Contains `NameMatcher`, `NoteBoostIndex`, `BoundedScoreHeap`, `FilterSql`,
//! and all scoring/filtering functions used by the search pipeline.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};

use crate::language::ChunkType;
use crate::math::cosine_similarity;
use crate::nl::tokenize_identifier;
use crate::note::path_matches_mention;
use crate::store::helpers::{NoteSummary, SearchFilter, SearchResult};

// ============ Scoring Configuration ============

/// Central configuration for all search scoring constants.
///
/// Consolidates name matching tiers, note boost factor, importance
/// demotion weights, and parent boost parameters into one struct.
/// Use `ScoringConfig::DEFAULT` everywhere — no scattered magic numbers.
pub(crate) struct ScoringConfig {
    pub name_exact: f32,
    pub name_contains: f32,
    pub name_contained_by: f32,
    pub name_max_overlap: f32,
    pub note_boost_factor: f32,
    pub importance_test: f32,
    pub importance_private: f32,
    pub parent_boost_per_child: f32,
    pub parent_boost_cap: f32,
}

impl ScoringConfig {
    pub const DEFAULT: Self = Self {
        name_exact: 1.0,
        name_contains: 0.8,
        name_contained_by: 0.6,
        name_max_overlap: 0.5,
        note_boost_factor: 0.15,
        importance_test: 0.70,
        importance_private: 0.80,
        parent_boost_per_child: 0.05,
        parent_boost_cap: 1.15,
    };
}

// ============ Name Matching ============

/// Detect whether a query looks like a code identifier vs natural language.
///
/// Name-like: "parseConfig", "handle_error", "CircuitBreaker"
/// NL-like: "function that handles errors", "how does parsing work"
///
/// Used to gate name_boost — boosting by name similarity is harmful for
/// NL queries because it rewards coincidental substring matches over
/// semantic relevance.
pub(crate) fn is_name_like_query(query: &str) -> bool {
    let words: Vec<&str> = query.split_whitespace().collect();
    // Single token or two-token queries are likely identifiers
    if words.len() <= 2 {
        return true;
    }
    // NL indicators: common function words that never appear in identifiers
    const NL_WORDS: &[&str] = &[
        "the",
        "a",
        "an",
        "is",
        "are",
        "was",
        "were",
        "that",
        "which",
        "how",
        "what",
        "where",
        "when",
        "does",
        "do",
        "can",
        "should",
        "would",
        "could",
        "for",
        "with",
        "from",
        "into",
        "this",
        "these",
        "those",
        "function",
        "method",
        "code",
        "implement",
        "find",
        "search",
    ];
    let lower = query.to_lowercase();
    let lower_words: Vec<&str> = lower.split_whitespace().collect();
    for w in &lower_words {
        if NL_WORDS.contains(w) {
            return false;
        }
    }
    // 3+ words with no NL indicators — still likely NL if all lowercase
    // (identifiers are usually camelCase or snake_case)
    if words.len() >= 3 && lower == query && !query.contains('_') {
        return false;
    }
    true
}

/// Pre-tokenized query for efficient name matching in loops
///
/// Create once before iterating over search results, then call `score()` for each name.
/// Avoids re-tokenizing the query for every result.
pub(crate) struct NameMatcher {
    query_lower: String,
    query_words: Vec<String>,
}

impl NameMatcher {
    /// Create a new matcher with pre-tokenized query
    pub fn new(query: &str) -> Self {
        Self {
            query_lower: query.to_lowercase(),
            query_words: tokenize_identifier(query)
                .into_iter()
                .map(|w| w.to_lowercase())
                .collect(),
        }
    }

    /// Compute name match score against pre-tokenized query
    pub fn score(&self, name: &str) -> f32 {
        let cfg = &ScoringConfig::DEFAULT;
        let name_lower = name.to_lowercase();

        // Exact match
        if name_lower == self.query_lower {
            return cfg.name_exact;
        }

        // Name contains query as substring
        if name_lower.contains(&self.query_lower) {
            return cfg.name_contains;
        }

        // Query contains name as substring
        if self.query_lower.contains(&name_lower) {
            return cfg.name_contained_by;
        }

        // Word overlap scoring
        if self.query_words.is_empty() {
            return 0.0;
        }

        // Trade-off: Building name_words Vec per result adds allocation overhead,
        // but pre-indexing names would require storing tokenized names in the DB
        // (increasing schema complexity and storage ~20%). Given name_words are
        // typically 1-5 words and this only runs for top-N results after filtering,
        // the per-result allocation is acceptable.
        let name_words: Vec<String> = tokenize_identifier(name)
            .into_iter()
            .map(|w| w.to_lowercase())
            .collect();

        if name_words.is_empty() {
            return 0.0;
        }

        // Fast path: build HashSet for O(1) exact match lookup
        let name_word_set: HashSet<&str> = name_words.iter().map(String::as_str).collect();

        // O(m*n) substring matching trade-off:
        // - m = query words (typically 1-5), n = name words (typically 1-5)
        // - Worst case: ~25 comparisons per name, but short-circuits on exact match
        // - Alternative (pre-indexing substring tries) would add complexity for minimal gain
        //   since names are short and search results are already capped by limit
        let overlap = self
            .query_words
            .iter()
            .filter(|w| {
                // Fast path: exact word match
                if name_word_set.contains(w.as_str()) {
                    return true;
                }
                // Slow path: substring matching (only if no exact match)
                // Intentionally excludes equal-length substrings: if lengths are equal
                // but strings differ, they're not substrings of each other (would need
                // exact match, handled above). This avoids redundant contains() calls.
                name_words.iter().any(|nw| {
                    // Short-circuit: check length before expensive substring search
                    (nw.len() > w.len() && nw.contains(w.as_str()))
                        || (w.len() > nw.len() && w.contains(nw.as_str()))
                })
            })
            .count() as f32;
        let total = self.query_words.len().max(1) as f32;

        (overlap / total) * cfg.name_max_overlap
    }
}

/// Extract file path from a chunk ID.
///
/// Standard format: `"path:line_start:hash_prefix"` (3 segments from right)
/// Windowed format: `"path:line_start:hash_prefix:wN"` (4 segments)
///
/// The hash_prefix is always 8 hex chars. Windowed chunk IDs append `:wN` where
/// N is a small integer (0-99). We detect windowed IDs by checking if the last
/// segment starts with 'w' followed by digits.
pub(crate) fn extract_file_from_chunk_id(id: &str) -> &str {
    // Strip last segment
    let Some(last_colon) = id.rfind(':') else {
        return id;
    };
    let last_seg = &id[last_colon + 1..];

    // Determine how many segments to strip from the right:
    // - Standard: 2 (hash_prefix, line_start)
    // - Windowed: 3 (wN, hash_prefix, line_start)
    // Window suffix format: "w0", "w1", ..., "w99"
    let segments_to_strip = if !last_seg.is_empty()
        && last_seg.starts_with('w')
        && last_seg.len() <= 3
        && last_seg[1..].bytes().all(|b| b.is_ascii_digit())
    {
        3
    } else {
        2
    };

    let mut end = id.len();
    for _ in 0..segments_to_strip {
        if let Some(i) = id[..end].rfind(':') {
            end = i;
        } else {
            break;
        }
    }
    &id[..end]
}

/// Compile a glob pattern into a matcher, logging and ignoring invalid patterns.
///
/// Returns `None` if the pattern is `None` or invalid (with a warning logged).
pub(crate) fn compile_glob_filter(pattern: Option<&String>) -> Option<globset::GlobMatcher> {
    pattern.and_then(|p| match globset::Glob::new(p) {
        Ok(g) => Some(g.compile_matcher()),
        Err(e) => {
            tracing::warn!(pattern = %p, error = %e, "Invalid glob pattern, ignoring filter");
            None
        }
    })
}

/// Compute name match score for hybrid search
///
/// For repeated calls with the same query, use `NameMatcher::new(query).score(name)` instead.
#[cfg(test)]
pub(crate) fn name_match_score(query: &str, name: &str) -> f32 {
    NameMatcher::new(query).score(name)
}

/// Compute the note-based score boost for a chunk.
///
/// Checks if any note's mentions match the chunk's file path or name.
/// When multiple notes match, takes the strongest absolute sentiment
/// (preserving sign) to avoid averaging away strong signals.
///
/// Returns a multiplier: `1.0 + sentiment * ScoringConfig::DEFAULT.note_boost_factor`
///
/// Production code uses [`NoteBoostIndex::boost`] for amortized O(1) lookups.
/// This function is retained for unit tests.
#[cfg(test)]
fn note_boost(file_path: &str, chunk_name: &str, notes: &[NoteSummary]) -> f32 {
    let mut strongest: Option<f32> = None;
    for note in notes {
        for mention in &note.mentions {
            if path_matches_mention(file_path, mention) || chunk_name == mention {
                match strongest {
                    Some(prev) if note.sentiment.abs() > prev.abs() => {
                        strongest = Some(note.sentiment);
                    }
                    None => {
                        strongest = Some(note.sentiment);
                    }
                    _ => {}
                }
                break; // This note already matched, check next note
            }
        }
    }
    match strongest {
        Some(s) => 1.0 + s * ScoringConfig::DEFAULT.note_boost_factor,
        None => 1.0,
    }
}

/// Pre-computed note boost lookup for O(1) name matching and reduced path scans.
///
/// Built once from notes before the scoring loop, amortizing the O(notes x mentions)
/// cost across all chunks. Name mentions use exact HashMap lookup (O(1)).
/// Path mentions are stored separately for suffix/prefix matching, but with only
/// the path-type mentions instead of all mentions.
pub(crate) struct NoteBoostIndex<'a> {
    /// Exact name -> strongest sentiment (absolute value wins, preserving sign)
    #[cfg(test)]
    pub(super) name_sentiments: HashMap<&'a str, f32>,
    #[cfg(not(test))]
    name_sentiments: HashMap<&'a str, f32>,
    /// (mention_str, sentiment) pairs for path-based mentions
    #[cfg(test)]
    pub(super) path_mentions: Vec<(&'a str, f32)>,
    #[cfg(not(test))]
    path_mentions: Vec<(&'a str, f32)>,
}

impl<'a> NoteBoostIndex<'a> {
    /// Build the lookup index from notes. O(notes x mentions), done once.
    pub fn new(notes: &'a [NoteSummary]) -> Self {
        let mut name_sentiments: HashMap<&'a str, f32> = HashMap::new();
        let mut path_mentions: Vec<(&'a str, f32)> = Vec::new();

        for note in notes {
            for mention in &note.mentions {
                // Heuristic: mentions containing '/' or '.' or '\' are path-like,
                // others are name-like (exact match on chunk name)
                let is_path_like =
                    mention.contains('/') || mention.contains('.') || mention.contains('\\');
                if is_path_like {
                    path_mentions.push((mention.as_str(), note.sentiment));
                } else {
                    let entry = name_sentiments.entry(mention.as_str()).or_insert(0.0);
                    if note.sentiment.abs() > entry.abs() {
                        *entry = note.sentiment;
                    }
                }
            }
        }

        // AC-11: Deduplicate path mentions — keep strongest sentiment per mention string
        let mut deduped_paths: HashMap<&'a str, f32> = HashMap::new();
        for (mention, sentiment) in &path_mentions {
            let entry = deduped_paths.entry(mention).or_insert(0.0);
            if sentiment.abs() > entry.abs() {
                *entry = *sentiment;
            }
        }
        let path_mentions: Vec<(&'a str, f32)> = deduped_paths.into_iter().collect();

        Self {
            name_sentiments,
            path_mentions,
        }
    }

    /// Compute the note-based score boost for a chunk.
    ///
    /// Checks name mentions via HashMap lookup (O(1)), then scans path mentions
    /// for suffix/prefix matches. Takes strongest absolute sentiment across all
    /// matches (preserving sign).
    ///
    /// Returns a multiplier: `1.0 + sentiment * note_boost_factor`
    #[inline]
    pub fn boost(&self, file_path: &str, chunk_name: &str) -> f32 {
        let mut strongest: Option<f32> = None;

        // O(1) name lookup
        if let Some(&sentiment) = self.name_sentiments.get(chunk_name) {
            strongest = Some(sentiment);
        }

        // Path mention scan (only path-like mentions, not all mentions)
        for &(mention, sentiment) in &self.path_mentions {
            if path_matches_mention(file_path, mention) {
                match strongest {
                    Some(prev) if sentiment.abs() > prev.abs() => {
                        strongest = Some(sentiment);
                    }
                    None => {
                        strongest = Some(sentiment);
                    }
                    _ => {}
                }
            }
        }

        match strongest {
            Some(s) => 1.0 + s * ScoringConfig::DEFAULT.note_boost_factor,
            None => 1.0,
        }
    }
}

/// Compute search-time importance multiplier for a chunk.
///
/// Demotes test functions (via [`is_test_chunk`](crate::is_test_chunk)) and
/// underscore-prefixed private helpers.
/// Applied as a multiplier like `note_boost`, so it composes: `score * note_boost * importance`.
///
/// | Signal                   | Detection                           | Multiplier |
/// |--------------------------|-------------------------------------|------------|
/// | Test chunk               | `crate::is_test_chunk(name, path)`  | 0.70       |
/// | Underscore-prefixed      | name starts with `_` (not `__`)     | 0.80       |
///
/// Returns 1.0 (no change) when demotion doesn't apply.
pub(crate) fn chunk_importance(name: &str, file_path: &str) -> f32 {
    let cfg = &ScoringConfig::DEFAULT;
    if crate::is_test_chunk(name, file_path) {
        return cfg.importance_test;
    }
    // Underscore-prefixed private (but not dunder like __init__)
    if name.starts_with('_') && !name.starts_with("__") {
        return cfg.importance_private;
    }
    1.0
}

/// Boost container chunks (Class, Struct, Interface) when multiple child methods
/// from the same parent appear in search results.
///
/// When a query semantically matches several methods of one class, the class
/// itself is usually the best answer — the methods individually match fragments
/// of the query, but the class embodies the whole concept (e.g., "circuit breaker
/// pattern" → `CircuitBreaker` class, not `recordFailure` method).
///
/// Algorithm: count how many results have `parent_type_name == X`. If a
/// Class/Struct/Interface chunk named `X` also appears in results, boost it.
///
/// Boost magnitude: `1.0 + parent_boost_per_child × (child_count - 1)`, capped at `parent_boost_cap`.
/// With 2 children → 1.05×, 3 → 1.10×, 4+ → 1.15×.
///
/// Re-sorts results by score after boosting.
pub(crate) fn apply_parent_boost(results: &mut [SearchResult]) {
    if results.len() < 3 {
        return; // Need at least a container + 2 children
    }

    // Count how many results share each parent_type_name
    let mut parent_counts: HashMap<String, usize> = HashMap::new();
    for r in results.iter() {
        if let Some(ref ptn) = r.chunk.parent_type_name {
            *parent_counts.entry(ptn.clone()).or_insert(0) += 1;
        }
    }

    // Only proceed if any parent_type_name appears 2+ times
    if !parent_counts.values().any(|&c| c >= 2) {
        return;
    }

    let cfg = &ScoringConfig::DEFAULT;
    let max_children = (cfg.parent_boost_cap - 1.0) / cfg.parent_boost_per_child;
    let mut boosted = false;
    for r in results.iter_mut() {
        let is_container = matches!(
            r.chunk.chunk_type,
            ChunkType::Class | ChunkType::Struct | ChunkType::Interface
        );
        if !is_container {
            continue;
        }
        if let Some(&count) = parent_counts.get(&r.chunk.name) {
            if count >= 2 {
                let boost =
                    1.0 + cfg.parent_boost_per_child * (count as f32 - 1.0).min(max_children);
                tracing::debug!(
                    name = %r.chunk.name,
                    child_count = count,
                    boost = %boost,
                    "parent_boost: boosting container"
                );
                r.score *= boost;
                boosted = true;
            }
        }
    }

    if boosted {
        results.sort_by(|a, b| b.score.total_cmp(&a.score));
    }
}

/// Bounded min-heap for maintaining top-N search results by score.
///
/// Uses a min-heap internally so the smallest score is always at the top,
/// allowing O(log N) eviction when the heap is full. This bounds memory to
/// O(limit) instead of O(total_chunks) for the scoring phase.
pub(crate) struct BoundedScoreHeap {
    heap: BinaryHeap<Reverse<(OrderedFloat, String)>>,
    capacity: usize,
}

/// Wrapper for f32 that implements Ord for use in BinaryHeap.
/// Uses total_cmp for consistent ordering (NaN sorts to the end).
#[derive(Clone, Copy, PartialEq)]
struct OrderedFloat(f32);

impl Eq for OrderedFloat {}

impl PartialOrd for OrderedFloat {
    /// Compares two values and returns an ordering, wrapped in `Option`.
    ///
    /// # Arguments
    ///
    /// * `other` - The value to compare against
    ///
    /// # Returns
    ///
    /// Returns `Some(Ordering)` indicating whether `self` is less than, equal to, or greater than `other`.
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OrderedFloat {
    /// Compares two values using total ordering on their inner floating-point values.
    ///
    /// # Arguments
    ///
    /// * `other` - The value to compare against
    ///
    /// # Returns
    ///
    /// An `Ordering` indicating whether `self` is less than, equal to, or greater than `other`. Uses total ordering semantics where NaN values are comparable.
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&other.0)
    }
}

impl BoundedScoreHeap {
    /// Creates a new bounded priority queue with the specified capacity.
    ///
    /// # Arguments
    ///
    /// * `capacity` - The maximum number of elements the queue can hold
    ///
    /// # Returns
    ///
    /// A new `BoundedPriorityQueue` instance with the given capacity. The internal heap is pre-allocated with space for `capacity + 1` elements.
    pub fn new(capacity: usize) -> Self {
        Self {
            heap: BinaryHeap::with_capacity(capacity + 1),
            capacity,
        }
    }

    /// Push a scored result. If at capacity, evicts the lowest score.
    pub fn push(&mut self, id: String, score: f32) {
        if !score.is_finite() {
            tracing::warn!("BoundedScoreHeap: ignoring non-finite score");
            return;
        }

        // If below capacity, always insert
        if self.heap.len() < self.capacity {
            self.heap.push(Reverse((OrderedFloat(score), id)));
            return;
        }

        // At capacity - only insert if strictly better than current minimum.
        // Using > (not >=) gives first-indexed stability: when scores are equal,
        // earlier items are kept. This prevents last-wins bias where later-indexed
        // chunks systematically replace earlier ones at equal scores.
        if let Some(Reverse((OrderedFloat(min_score), _))) = self.heap.peek() {
            if score > *min_score {
                self.heap.pop();
                self.heap.push(Reverse((OrderedFloat(score), id)));
            }
        }
    }

    /// Drain into a sorted Vec (highest score first).
    pub fn into_sorted_vec(self) -> Vec<(String, f32)> {
        let mut results: Vec<_> = self
            .heap
            .into_iter()
            .map(|Reverse((OrderedFloat(score), id))| (id, score))
            .collect();
        results.sort_by(|a, b| b.1.total_cmp(&a.1));
        results
    }
}

/// Result of assembling SQL WHERE conditions from a [`SearchFilter`].
///
/// Separates filter analysis (testable without a database) from SQL execution.
/// The caller combines these pieces with cursor-specific clauses (rowid, LIMIT).
pub(crate) struct FilterSql {
    /// SQL WHERE conditions (e.g., `"language IN (?1,?2)"`)
    pub conditions: Vec<String>,
    /// Bind values corresponding to the placeholders in `conditions`, in order
    pub bind_values: Vec<String>,
    /// Column list for SELECT (includes `name` when hybrid scoring or demotion is needed)
    pub columns: &'static str,
    /// Whether hybrid name+embedding scoring is active
    pub use_hybrid: bool,
    /// Whether RRF fusion with FTS keyword search is active
    pub use_rrf: bool,
}

/// Build SQL filter components from a [`SearchFilter`].
///
/// Pure function — no database access. Returns conditions, bind values, and
/// the column list needed for the scoring loop. Bind parameter indices are
/// 1-based and contiguous.
pub(crate) fn build_filter_sql(filter: &SearchFilter) -> FilterSql {
    let mut conditions = Vec::new();
    let mut bind_values: Vec<String> = Vec::new();

    if let Some(ref langs) = filter.languages {
        let placeholders: Vec<_> = (0..langs.len())
            .map(|i| format!("?{}", bind_values.len() + i + 1))
            .collect();
        conditions.push(format!("language IN ({})", placeholders.join(",")));
        for lang in langs {
            bind_values.push(lang.to_string());
        }
    }

    if let Some(ref types) = filter.chunk_types {
        let placeholders: Vec<_> = (0..types.len())
            .map(|i| format!("?{}", bind_values.len() + i + 1))
            .collect();
        conditions.push(format!("chunk_type IN ({})", placeholders.join(",")));
        for ct in types {
            bind_values.push(ct.to_string());
        }
    }

    let use_hybrid = filter.name_boost > 0.0
        && !filter.query_text.is_empty()
        && is_name_like_query(&filter.query_text);
    let use_rrf = filter.enable_rrf && !filter.query_text.is_empty();

    // Select columns: always id + embedding, optionally name for hybrid scoring
    // or demotion (test function detection needs the name)
    let need_name = use_hybrid || filter.enable_demotion;
    let columns = if need_name {
        "rowid, id, embedding, name"
    } else {
        "rowid, id, embedding"
    };

    FilterSql {
        conditions,
        bind_values,
        columns,
        use_hybrid,
        use_rrf,
    }
}

/// Score a single candidate chunk against the query.
///
/// Pure function — no database access. Combines embedding similarity, optional
/// name boosting, glob filtering, note boosting, and test-function demotion.
///
/// Returns `None` if the candidate is filtered out (glob mismatch or below threshold).
#[allow(clippy::too_many_arguments)]
pub(crate) fn score_candidate(
    embedding: &[f32],
    query: &[f32],
    name: Option<&str>,
    file_part: &str,
    filter: &SearchFilter,
    name_matcher: Option<&NameMatcher>,
    glob_matcher: Option<&globset::GlobMatcher>,
    note_index: &NoteBoostIndex<'_>,
    threshold: f32,
) -> Option<f32> {
    let embedding_score = cosine_similarity(query, embedding)?;

    let base_score = if let Some(matcher) = name_matcher {
        let n = name.unwrap_or("");
        let name_score = matcher.score(n);
        (1.0 - filter.name_boost) * embedding_score + filter.name_boost * name_score
    } else {
        embedding_score
    };

    if let Some(matcher) = glob_matcher {
        if !matcher.is_match(file_part) {
            return None;
        }
    }

    // Apply note-based boost: notes mentioning this chunk's file or name
    // adjust its score by up to ±15%
    let chunk_name = name.unwrap_or("");
    let mut score = base_score * note_index.boost(file_part, chunk_name);

    // Apply demotion for test functions and underscore-prefixed names
    if filter.enable_demotion {
        score *= chunk_importance(chunk_name, file_part);
    }

    if score >= threshold {
        Some(score)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ===== name_match_score tests =====

    #[test]
    fn test_name_match_exact() {
        assert_eq!(name_match_score("parse", "parse"), 1.0);
    }

    #[test]
    fn test_name_match_contains() {
        assert_eq!(name_match_score("parse", "parseConfig"), 0.8);
    }

    #[test]
    fn test_name_match_contained() {
        assert_eq!(name_match_score("parseConfigFile", "parse"), 0.6);
    }

    #[test]
    fn test_name_match_partial_overlap() {
        let score = name_match_score("parseConfig", "configParser");
        assert!(score > 0.0 && score <= 0.5);
    }

    #[test]
    fn test_name_match_no_match() {
        assert_eq!(name_match_score("foo", "bar"), 0.0);
    }

    // ===== note_boost tests =====

    /// Creates a test `NoteSummary` with the provided sentiment score and mentions.
    ///
    /// # Arguments
    ///
    /// * `sentiment` - A floating-point sentiment score to assign to the note.
    /// * `mentions` - A slice of string references representing entities or topics mentioned in the note.
    ///
    /// # Returns
    ///
    /// A `NoteSummary` struct with a fixed test ID ("note:test"), fixed test text ("test note"), the provided sentiment score, and the mentions converted to owned `String` values.
    fn make_note(sentiment: f32, mentions: &[&str]) -> NoteSummary {
        NoteSummary {
            id: "note:test".to_string(),
            text: "test note".to_string(),
            sentiment,
            mentions: mentions.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn test_note_boost_no_notes() {
        let boost = note_boost("src/lib.rs", "my_fn", &[]);
        assert_eq!(boost, 1.0);
    }

    #[test]
    fn test_note_boost_no_match() {
        let notes = vec![make_note(-0.5, &["other.rs"])];
        let boost = note_boost("src/lib.rs", "my_fn", &notes);
        assert_eq!(boost, 1.0);
    }

    #[test]
    fn test_note_boost_file_match_negative() {
        let notes = vec![make_note(-1.0, &["lib.rs"])];
        let boost = note_boost("src/lib.rs", "my_fn", &notes);
        assert!(
            (boost - 0.85).abs() < 0.001,
            "Expected ~0.85, got {}",
            boost
        );
    }

    #[test]
    fn test_note_boost_file_match_positive() {
        let notes = vec![make_note(1.0, &["lib.rs"])];
        let boost = note_boost("src/lib.rs", "my_fn", &notes);
        assert!(
            (boost - 1.15).abs() < 0.001,
            "Expected ~1.15, got {}",
            boost
        );
    }

    #[test]
    fn test_note_boost_name_match() {
        let notes = vec![make_note(0.5, &["my_fn"])];
        let boost = note_boost("src/lib.rs", "my_fn", &notes);
        assert!(
            (boost - 1.075).abs() < 0.001,
            "Expected ~1.075, got {}",
            boost
        );
    }

    #[test]
    fn test_note_boost_strongest_wins() {
        // Two notes: weak positive and strong negative. Strong negative should win.
        let notes = vec![make_note(0.5, &["lib.rs"]), make_note(-1.0, &["lib.rs"])];
        let boost = note_boost("src/lib.rs", "my_fn", &notes);
        assert!(
            (boost - 0.85).abs() < 0.001,
            "Expected ~0.85, got {}",
            boost
        );
    }

    #[test]
    fn test_note_boost_strongest_absolute_preserves_sign() {
        // Two notes: strong positive and weak negative. Strong positive should win.
        let notes = vec![make_note(1.0, &["lib.rs"]), make_note(-0.5, &["lib.rs"])];
        let boost = note_boost("src/lib.rs", "my_fn", &notes);
        assert!(
            (boost - 1.15).abs() < 0.001,
            "Expected ~1.15, got {}",
            boost
        );
    }

    // ===== compile_glob_filter tests =====

    #[test]
    fn test_compile_glob_filter_none() {
        assert!(compile_glob_filter(None).is_none());
    }

    #[test]
    fn test_compile_glob_filter_valid() {
        let pattern = "src/**/*.rs".to_string();
        let matcher = compile_glob_filter(Some(&pattern));
        assert!(matcher.is_some());
        let m = matcher.unwrap();
        assert!(m.is_match("src/cli/mod.rs"));
        assert!(!m.is_match("tests/foo.py"));
    }

    #[test]
    fn test_compile_glob_filter_invalid() {
        let pattern = "[invalid".to_string();
        assert!(compile_glob_filter(Some(&pattern)).is_none());
    }

    // ===== extract_file_from_chunk_id tests =====

    #[test]
    fn test_extract_file_standard_chunk_id() {
        // Standard: "path:line_start:hash_prefix"
        assert_eq!(
            extract_file_from_chunk_id("src/foo.rs:10:abc12345"),
            "src/foo.rs"
        );
    }

    #[test]
    fn test_extract_file_windowed_chunk_id() {
        // Windowed: "path:line_start:hash_prefix:wN"
        assert_eq!(
            extract_file_from_chunk_id("src/foo.rs:10:abc12345:w0"),
            "src/foo.rs"
        );
        assert_eq!(
            extract_file_from_chunk_id("src/foo.rs:10:abc12345:w3"),
            "src/foo.rs"
        );
    }

    #[test]
    fn test_extract_file_nested_path() {
        assert_eq!(
            extract_file_from_chunk_id("src/cli/commands/mod.rs:42:deadbeef"),
            "src/cli/commands/mod.rs"
        );
        assert_eq!(
            extract_file_from_chunk_id("src/cli/commands/mod.rs:42:deadbeef:w1"),
            "src/cli/commands/mod.rs"
        );
    }

    #[test]
    fn test_extract_file_windowed_chunk_id_w_prefix() {
        // Windowed IDs use "wN" format (not bare digits)
        assert_eq!(
            extract_file_from_chunk_id("src/foo.rs:10:abc12345:w0"),
            "src/foo.rs"
        );
        assert_eq!(
            extract_file_from_chunk_id("src/foo.rs:10:abc12345:w12"),
            "src/foo.rs"
        );
    }

    #[test]
    fn test_extract_file_hash_not_confused_with_window() {
        // 8-char hex hash should NOT be mistaken for a window index
        assert_eq!(
            extract_file_from_chunk_id("src/foo.rs:10:deadbeef"),
            "src/foo.rs"
        );
    }

    #[test]
    fn test_extract_file_no_colons() {
        assert_eq!(extract_file_from_chunk_id("justanid"), "justanid");
    }

    // ===== BoundedScoreHeap tests =====

    #[test]
    fn test_bounded_heap_equal_scores() {
        let mut heap = BoundedScoreHeap::new(2);
        heap.push("a".to_string(), 0.5);
        heap.push("b".to_string(), 0.5);
        heap.push("c".to_string(), 0.5);
        let results = heap.into_sorted_vec();
        assert_eq!(results.len(), 2);
        // First-indexed stability: equal scores don't replace existing entries,
        // so "a" and "b" are kept, "c" is rejected.
        assert!(results.iter().any(|(id, _)| id == "a"));
        assert!(results.iter().any(|(id, _)| id == "b"));
    }

    // ===== BoundedScoreHeap additional tests =====

    #[test]
    fn test_bounded_heap_evicts_lowest() {
        let mut heap = BoundedScoreHeap::new(2);
        heap.push("low".to_string(), 0.1);
        heap.push("mid".to_string(), 0.5);
        heap.push("high".to_string(), 0.9);
        let results = heap.into_sorted_vec();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, "high");
        assert_eq!(results[1].0, "mid");
    }

    #[test]
    fn test_bounded_heap_ignores_non_finite() {
        let mut heap = BoundedScoreHeap::new(5);
        heap.push("nan".to_string(), f32::NAN);
        heap.push("inf".to_string(), f32::INFINITY);
        heap.push("neginf".to_string(), f32::NEG_INFINITY);
        heap.push("ok".to_string(), 0.5);
        let results = heap.into_sorted_vec();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "ok");
    }

    #[test]
    fn test_bounded_heap_empty() {
        let heap = BoundedScoreHeap::new(5);
        let results = heap.into_sorted_vec();
        assert!(results.is_empty());
    }

    // ===== parent_boost tests =====

    /// Constructs a test SearchResult with minimal required fields populated.
    ///
    /// # Arguments
    ///
    /// * `name` - The identifier used for the chunk's id and name
    /// * `chunk_type` - The type classification of the code chunk
    /// * `parent_type_name` - Optional name of the parent type or scope
    /// * `score` - The relevance score for this search result
    ///
    /// # Returns
    ///
    /// A SearchResult containing a ChunkSummary with test defaults (TypeScript language, test.ts file, lines 1-10) and the provided score.
    fn make_result(
        name: &str,
        chunk_type: ChunkType,
        parent_type_name: Option<&str>,
        score: f32,
    ) -> SearchResult {
        use crate::store::helpers::ChunkSummary;
        SearchResult {
            chunk: ChunkSummary {
                id: name.to_string(),
                file: std::path::PathBuf::from("test.ts"),
                language: crate::parser::Language::TypeScript,
                chunk_type,
                name: name.to_string(),
                signature: String::new(),
                content: String::new(),
                doc: None,
                line_start: 1,
                line_end: 10,
                parent_id: None,
                parent_type_name: parent_type_name.map(|s| s.to_string()),
                content_hash: String::new(),
                window_idx: None,
            },
            score,
        }
    }

    #[test]
    fn test_parent_boost_circuit_breaker() {
        // CircuitBreaker class at rank 4, its methods rank 1-3
        let mut results = vec![
            make_result(
                "recordFailure",
                ChunkType::Method,
                Some("CircuitBreaker"),
                0.88,
            ),
            make_result(
                "retryWithBackoff",
                ChunkType::Method,
                Some("CircuitBreaker"),
                0.86,
            ),
            make_result(
                "shouldAllow",
                ChunkType::Method,
                Some("CircuitBreaker"),
                0.85,
            ),
            make_result("CircuitBreaker", ChunkType::Class, None, 0.82),
        ];
        apply_parent_boost(&mut results);
        // 3 children → boost = 1.10, 0.82 * 1.10 = 0.902 > 0.88
        assert_eq!(results[0].chunk.name, "CircuitBreaker");
        assert!(results[0].score > 0.90);
    }

    #[test]
    fn test_parent_boost_no_effect_on_standalone_functions() {
        // Sort variants — standalone functions, no parent_type_name
        let mut results = vec![
            make_result("_insertionSortSmall", ChunkType::Function, None, 0.88),
            make_result("insertionSort", ChunkType::Function, None, 0.85),
            make_result("mergeSort", ChunkType::Function, None, 0.80),
        ];
        let scores_before: Vec<f32> = results.iter().map(|r| r.score).collect();
        apply_parent_boost(&mut results);
        let scores_after: Vec<f32> = results.iter().map(|r| r.score).collect();
        assert_eq!(scores_before, scores_after);
    }

    #[test]
    fn test_parent_boost_needs_minimum_two_children() {
        // Only 1 method from the class — no boost
        let mut results = vec![
            make_result(
                "recordFailure",
                ChunkType::Method,
                Some("CircuitBreaker"),
                0.88,
            ),
            make_result("CircuitBreaker", ChunkType::Class, None, 0.82),
            make_result("unrelatedFn", ChunkType::Function, None, 0.80),
        ];
        apply_parent_boost(&mut results);
        // CircuitBreaker should stay at rank 2
        assert_eq!(results[0].chunk.name, "recordFailure");
        assert_eq!(results[1].chunk.name, "CircuitBreaker");
    }

    #[test]
    fn test_parent_boost_caps_at_1_15() {
        // 5 children → should cap at 1.15, not 1.20
        let mut results = vec![
            make_result("m1", ChunkType::Method, Some("BigClass"), 0.88),
            make_result("m2", ChunkType::Method, Some("BigClass"), 0.87),
            make_result("m3", ChunkType::Method, Some("BigClass"), 0.86),
            make_result("m4", ChunkType::Method, Some("BigClass"), 0.85),
            make_result("m5", ChunkType::Method, Some("BigClass"), 0.84),
            make_result("BigClass", ChunkType::Class, None, 0.78),
        ];
        apply_parent_boost(&mut results);
        // max boost = 1.15, 0.78 * 1.15 = 0.897
        let class_score = results
            .iter()
            .find(|r| r.chunk.name == "BigClass")
            .unwrap()
            .score;
        assert!(
            (class_score - 0.897).abs() < 0.001,
            "Expected ~0.897, got {class_score}"
        );
    }

    #[test]
    fn test_parent_boost_too_few_results() {
        // Only 2 results — function returns early
        let mut results = vec![
            make_result("foo", ChunkType::Method, Some("Bar"), 0.88),
            make_result("Bar", ChunkType::Class, None, 0.82),
        ];
        let score_before = results[1].score;
        apply_parent_boost(&mut results);
        assert_eq!(results[1].score, score_before);
    }

    // ===== chunk_importance tests =====

    #[test]
    fn test_chunk_importance_normal() {
        assert_eq!(chunk_importance("parse_config", "src/lib.rs"), 1.0);
    }

    #[test]
    fn test_chunk_importance_test_prefix() {
        assert_eq!(chunk_importance("test_parse_config", "src/lib.rs"), 0.70);
    }

    #[test]
    fn test_chunk_importance_test_upper() {
        // Go convention: TestFoo
        assert_eq!(
            chunk_importance("TestParseConfig", "src/lib.go"),
            ScoringConfig::DEFAULT.importance_test
        );
    }

    #[test]
    fn test_chunk_importance_underscore() {
        assert_eq!(
            chunk_importance("_helper", "src/lib.rs"),
            ScoringConfig::DEFAULT.importance_private
        );
    }

    #[test]
    fn test_chunk_importance_dunder_not_demoted() {
        // Python dunders like __init__ should NOT be demoted
        assert_eq!(chunk_importance("__init__", "src/lib.py"), 1.0);
    }

    #[test]
    fn test_chunk_importance_test_file() {
        // File named foo_test.rs → demotion via filename
        assert_eq!(
            chunk_importance("helper_fn", "src/foo_test.rs"),
            ScoringConfig::DEFAULT.importance_test
        );
    }

    #[test]
    fn test_chunk_importance_test_dir_demoted() {
        // Files in tests/ directory are test infrastructure → demoted
        assert_eq!(
            chunk_importance("real_fn", "tests/fixtures/eval.rs"),
            ScoringConfig::DEFAULT.importance_test
        );
    }

    #[test]
    fn test_chunk_importance_test_name_beats_path() {
        // test_ name triggers demotion even in normal directory
        assert_eq!(
            chunk_importance("test_foo", "src/lib.rs"),
            ScoringConfig::DEFAULT.importance_test
        );
    }

    // ===== is_name_like_query tests =====

    #[test]
    fn test_name_like_single_token() {
        assert!(is_name_like_query("parseConfig"));
        assert!(is_name_like_query("CircuitBreaker"));
        assert!(is_name_like_query("handle_error"));
    }

    #[test]
    fn test_name_like_two_tokens() {
        assert!(is_name_like_query("parse config"));
        assert!(is_name_like_query("error handler"));
    }

    #[test]
    fn test_nl_query_with_indicators() {
        assert!(!is_name_like_query("function that handles errors"));
        assert!(!is_name_like_query("how does parsing work"));
        assert!(!is_name_like_query("find error handling code"));
        assert!(!is_name_like_query("code that implements retry logic"));
    }

    #[test]
    fn test_nl_query_all_lowercase_3_plus_words() {
        assert!(!is_name_like_query("error handling retry"));
    }

    #[test]
    fn test_name_like_snake_case_multi() {
        // snake_case with 3+ words is still name-like
        assert!(is_name_like_query("handle_error_retry"));
    }

    // ===== build_filter_sql tests =====

    #[test]
    fn test_build_filter_sql_default() {
        let filter = SearchFilter::default();
        let fsql = build_filter_sql(&filter);
        assert!(fsql.conditions.is_empty());
        assert!(fsql.bind_values.is_empty());
        // Default has enable_demotion=true, which requires name column
        assert_eq!(fsql.columns, "rowid, id, embedding, name");
        assert!(!fsql.use_hybrid);
        assert!(!fsql.use_rrf);
    }

    #[test]
    fn test_build_filter_sql_no_name_column() {
        // Explicitly disable demotion + no hybrid → no name column needed
        let filter = SearchFilter {
            enable_demotion: false,
            ..Default::default()
        };
        let fsql = build_filter_sql(&filter);
        assert_eq!(fsql.columns, "rowid, id, embedding");
    }

    #[test]
    fn test_build_filter_sql_language_filter() {
        use crate::parser::Language;
        let filter = SearchFilter {
            languages: Some(vec![Language::Rust, Language::Python]),
            ..Default::default()
        };
        let fsql = build_filter_sql(&filter);
        assert_eq!(fsql.conditions.len(), 1);
        assert!(fsql.conditions[0].starts_with("language IN"));
        assert_eq!(fsql.bind_values.len(), 2);
        assert_eq!(fsql.bind_values[0], "rust");
        assert_eq!(fsql.bind_values[1], "python");
    }

    #[test]
    fn test_build_filter_sql_chunk_type_filter() {
        use crate::parser::ChunkType;
        let filter = SearchFilter {
            chunk_types: Some(vec![ChunkType::Function]),
            ..Default::default()
        };
        let fsql = build_filter_sql(&filter);
        assert_eq!(fsql.conditions.len(), 1);
        assert!(fsql.conditions[0].starts_with("chunk_type IN"));
        assert_eq!(fsql.bind_values.len(), 1);
    }

    #[test]
    fn test_build_filter_sql_combined_filters() {
        use crate::parser::{ChunkType, Language};
        let filter = SearchFilter {
            languages: Some(vec![Language::Rust]),
            chunk_types: Some(vec![ChunkType::Function, ChunkType::Method]),
            ..Default::default()
        };
        let fsql = build_filter_sql(&filter);
        assert_eq!(fsql.conditions.len(), 2);
        // 1 language + 2 chunk types = 3 bind values
        assert_eq!(fsql.bind_values.len(), 3);
        // Verify contiguous bind param indices: language gets ?1, chunk_types get ?2,?3
        assert!(fsql.conditions[0].contains("?1"));
        assert!(fsql.conditions[1].contains("?2"));
        assert!(fsql.conditions[1].contains("?3"));
    }

    #[test]
    fn test_build_filter_sql_hybrid_flags() {
        let filter = SearchFilter {
            name_boost: 0.3,
            query_text: "parse".to_string(),
            enable_rrf: true,
            ..Default::default()
        };
        let fsql = build_filter_sql(&filter);
        assert!(fsql.use_hybrid);
        assert!(fsql.use_rrf);
        // name needed for hybrid scoring
        assert!(fsql.columns.contains("name"));
    }

    #[test]
    fn test_build_filter_sql_demotion_includes_name() {
        let filter = SearchFilter {
            enable_demotion: true,
            ..Default::default()
        };
        let fsql = build_filter_sql(&filter);
        assert!(fsql.columns.contains("name"));
    }

    #[test]
    fn test_build_filter_sql_rrf_needs_query_text() {
        // RRF enabled but empty query text → use_rrf should be false
        let filter = SearchFilter {
            enable_rrf: true,
            query_text: String::new(),
            ..Default::default()
        };
        let fsql = build_filter_sql(&filter);
        assert!(!fsql.use_rrf);
    }

    // ===== score_candidate tests =====

    /// Build a normalized 768-dim test vector for score_candidate tests.
    fn test_embedding(seed: f32) -> Vec<f32> {
        let mut v = vec![seed; 768];
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in &mut v {
                *x /= norm;
            }
        }
        v
    }

    #[test]
    fn test_score_candidate_basic() {
        let emb = test_embedding(1.0);
        let query = test_embedding(1.0);
        let filter = SearchFilter::default();
        let note_index = NoteBoostIndex::new(&[]);

        let score = score_candidate(
            &emb,
            &query,
            None,
            "src/lib.rs",
            &filter,
            None,
            None,
            &note_index,
            0.0,
        );
        assert!(score.is_some());
        assert!(
            score.unwrap() > 0.9,
            "Self-similarity should be ~1.0, got {}",
            score.unwrap()
        );
    }

    #[test]
    fn test_score_candidate_below_threshold() {
        // Near-orthogonal vectors → low cosine similarity
        let emb = test_embedding(1.0);
        let query = test_embedding(-1.0);
        let filter = SearchFilter::default();
        let note_index = NoteBoostIndex::new(&[]);

        let score = score_candidate(
            &emb,
            &query,
            None,
            "src/lib.rs",
            &filter,
            None,
            None,
            &note_index,
            0.5,
        );
        assert!(
            score.is_none(),
            "Opposite vectors should be below 0.5 threshold"
        );
    }

    #[test]
    fn test_score_candidate_glob_filters() {
        let emb = test_embedding(1.0);
        let query = test_embedding(1.0);
        let filter = SearchFilter::default();
        let note_index = NoteBoostIndex::new(&[]);
        let glob = globset::Glob::new("src/**/*.rs").unwrap().compile_matcher();

        // Matching path
        let score = score_candidate(
            &emb,
            &query,
            None,
            "src/lib.rs",
            &filter,
            None,
            Some(&glob),
            &note_index,
            0.0,
        );
        assert!(score.is_some());

        // Non-matching path
        let score = score_candidate(
            &emb,
            &query,
            None,
            "tests/foo.py",
            &filter,
            None,
            Some(&glob),
            &note_index,
            0.0,
        );
        assert!(score.is_none());
    }

    #[test]
    fn test_score_candidate_name_boost() {
        let emb = test_embedding(1.0);
        let query = test_embedding(1.0);
        let filter_no_boost = SearchFilter::default();
        let filter_with_boost = SearchFilter {
            name_boost: 0.3,
            query_text: "parseConfig".to_string(),
            ..Default::default()
        };
        let note_index = NoteBoostIndex::new(&[]);
        let matcher = NameMatcher::new("parseConfig");

        let score_no = score_candidate(
            &emb,
            &query,
            Some("parseConfig"),
            "src/a.rs",
            &filter_no_boost,
            None,
            None,
            &note_index,
            0.0,
        )
        .unwrap();
        let score_yes = score_candidate(
            &emb,
            &query,
            Some("parseConfig"),
            "src/a.rs",
            &filter_with_boost,
            Some(&matcher),
            None,
            &note_index,
            0.0,
        )
        .unwrap();

        assert!(score_yes > 0.0);
        assert!(score_no > 0.0);
    }

    #[test]
    fn test_score_candidate_demotion() {
        let emb = test_embedding(1.0);
        let query = test_embedding(1.0);
        let note_index = NoteBoostIndex::new(&[]);

        let filter_no_demote = SearchFilter {
            enable_demotion: false,
            ..Default::default()
        };
        let filter_demote = SearchFilter {
            enable_demotion: true,
            ..Default::default()
        };

        let score_normal = score_candidate(
            &emb,
            &query,
            Some("real_fn"),
            "src/lib.rs",
            &filter_demote,
            None,
            None,
            &note_index,
            0.0,
        )
        .unwrap();
        let score_test = score_candidate(
            &emb,
            &query,
            Some("test_foo"),
            "src/lib.rs",
            &filter_demote,
            None,
            None,
            &note_index,
            0.0,
        )
        .unwrap();
        let score_no_demote = score_candidate(
            &emb,
            &query,
            Some("test_foo"),
            "src/lib.rs",
            &filter_no_demote,
            None,
            None,
            &note_index,
            0.0,
        )
        .unwrap();

        // With demotion, test_ function should score lower than normal
        assert!(score_test < score_normal, "test_ should be demoted");
        // Without demotion flag, test_ function scores the same as normal
        assert!(
            (score_no_demote - score_normal).abs() < 0.001,
            "No demotion without flag"
        );
    }

    #[test]
    fn test_score_candidate_note_boost() {
        let emb = test_embedding(1.0);
        let query = test_embedding(1.0);
        let filter = SearchFilter::default();

        let notes = vec![make_note(1.0, &["lib.rs"])];
        let note_index_boosted = NoteBoostIndex::new(&notes);
        let note_index_empty = NoteBoostIndex::new(&[]);

        let score_boosted = score_candidate(
            &emb,
            &query,
            Some("my_fn"),
            "src/lib.rs",
            &filter,
            None,
            None,
            &note_index_boosted,
            0.0,
        )
        .unwrap();
        let score_plain = score_candidate(
            &emb,
            &query,
            Some("my_fn"),
            "src/lib.rs",
            &filter,
            None,
            None,
            &note_index_empty,
            0.0,
        )
        .unwrap();

        assert!(
            score_boosted > score_plain,
            "Positive note should boost score"
        );
    }

    // ===== NoteBoostIndex tests (TC-2) =====

    #[test]
    fn test_note_boost_index_empty_notes() {
        let notes: Vec<NoteSummary> = vec![];
        let index = NoteBoostIndex::new(&notes);
        assert_eq!(index.boost("src/lib.rs", "my_fn"), 1.0);
    }

    #[test]
    fn test_note_boost_index_name_mention_positive() {
        let notes = vec![NoteSummary {
            id: "1".into(),
            text: "good pattern".into(),
            sentiment: 0.5,
            mentions: vec!["my_fn".into()],
        }];
        let index = NoteBoostIndex::new(&notes);
        let boost = index.boost("src/lib.rs", "my_fn");
        assert!(
            boost > 1.0,
            "Positive sentiment should boost > 1.0, got {boost}"
        );
        assert!((boost - (1.0 + 0.5 * ScoringConfig::DEFAULT.note_boost_factor)).abs() < 1e-6);
    }

    #[test]
    fn test_note_boost_index_name_mention_negative() {
        let notes = vec![NoteSummary {
            id: "1".into(),
            text: "buggy code".into(),
            sentiment: -1.0,
            mentions: vec!["broken_fn".into()],
        }];
        let index = NoteBoostIndex::new(&notes);
        let boost = index.boost("src/lib.rs", "broken_fn");
        assert!(
            boost < 1.0,
            "Negative sentiment should reduce score, got {boost}"
        );
        assert!((boost - (1.0 - 1.0 * ScoringConfig::DEFAULT.note_boost_factor)).abs() < 1e-6);
    }

    #[test]
    fn test_note_boost_index_path_mention() {
        let notes = vec![NoteSummary {
            id: "1".into(),
            text: "important file".into(),
            sentiment: 0.5,
            mentions: vec!["src/search.rs".into()],
        }];
        let index = NoteBoostIndex::new(&notes);

        // Path mention should match file containing the path
        let boost = index.boost("src/search.rs", "unrelated_fn");
        assert!(
            boost > 1.0,
            "Path mention should boost matching file, got {boost}"
        );

        // Non-matching path should not be boosted
        let no_boost = index.boost("src/lib.rs", "unrelated_fn");
        assert_eq!(no_boost, 1.0, "Non-matching path should not be boosted");
    }

    #[test]
    fn test_note_boost_index_strongest_absolute_wins() {
        let notes = vec![
            NoteSummary {
                id: "1".into(),
                text: "mildly good".into(),
                sentiment: 0.5,
                mentions: vec!["my_fn".into()],
            },
            NoteSummary {
                id: "2".into(),
                text: "very bad".into(),
                sentiment: -1.0,
                mentions: vec!["my_fn".into()],
            },
        ];
        let index = NoteBoostIndex::new(&notes);
        let boost = index.boost("src/lib.rs", "my_fn");
        // -1.0 has stronger absolute value than 0.5, so it should win
        assert!(
            boost < 1.0,
            "Stronger negative should win over weaker positive, got {boost}"
        );
        assert!((boost - (1.0 - 1.0 * ScoringConfig::DEFAULT.note_boost_factor)).abs() < 1e-6);
    }

    #[test]
    fn test_note_boost_index_name_vs_path_classification() {
        // "search.rs" contains '.' so it's path-like
        // "my_fn" has no separators so it's name-like
        let notes = vec![NoteSummary {
            id: "1".into(),
            text: "note".into(),
            sentiment: 0.5,
            mentions: vec!["my_fn".into(), "search.rs".into()],
        }];
        let index = NoteBoostIndex::new(&notes);

        // Name-like mention should only match chunk name, not file path
        assert!(index.name_sentiments.contains_key("my_fn"));
        assert!(!index.name_sentiments.contains_key("search.rs"));
        assert_eq!(index.path_mentions.len(), 1);
    }

    #[test]
    fn test_note_boost_index_no_match() {
        let notes = vec![NoteSummary {
            id: "1".into(),
            text: "specific note".into(),
            sentiment: 1.0,
            mentions: vec!["other_fn".into()],
        }];
        let index = NoteBoostIndex::new(&notes);
        assert_eq!(index.boost("src/lib.rs", "my_fn"), 1.0);
    }

    // ===== language/chunk_type filter set tests (TC-3) =====

    #[test]
    fn test_lang_filter_set_membership() {
        use crate::language::Language;
        let langs = vec![Language::Rust, Language::Python];
        let lang_set: HashSet<String> =
            langs.iter().map(|l| l.to_string().to_lowercase()).collect();
        assert!(lang_set.contains("rust"));
        assert!(lang_set.contains("python"));
        assert!(!lang_set.contains("typescript"));
        assert!(!lang_set.contains("go"));
    }

    #[test]
    fn test_chunk_type_filter_set_membership() {
        use crate::language::ChunkType;
        let types = vec![ChunkType::Function, ChunkType::Method];
        let type_set: HashSet<String> =
            types.iter().map(|t| t.to_string().to_lowercase()).collect();
        assert!(type_set.contains("function"));
        assert!(type_set.contains("method"));
        assert!(!type_set.contains("struct"));
        assert!(!type_set.contains("class"));
    }

    #[test]
    fn test_lang_filter_case_insensitive() {
        use crate::language::Language;
        let langs = vec![Language::Rust];
        let lang_set: HashSet<String> =
            langs.iter().map(|l| l.to_string().to_lowercase()).collect();
        // eq_ignore_ascii_case avoids per-candidate allocation (PERF-17)
        assert!(lang_set.iter().any(|l| "rust".eq_ignore_ascii_case(l)));
        assert!(lang_set.iter().any(|l| "Rust".eq_ignore_ascii_case(l)));
        assert!(!lang_set.iter().any(|l| "Python".eq_ignore_ascii_case(l)));
    }

    #[test]
    fn test_lang_filter_none_passes_all() {
        // When filter.languages is None, lang_set is None and all candidates pass
        let lang_set: Option<HashSet<String>> = None;
        let candidate_lang = "rust";
        let passes = lang_set.as_ref().map_or(true, |s| {
            s.iter().any(|l| candidate_lang.eq_ignore_ascii_case(l))
        });
        assert!(passes);
    }

    #[test]
    fn test_type_filter_none_passes_all() {
        // When filter.chunk_types is None, type_set is None and all candidates pass
        let type_set: Option<HashSet<String>> = None;
        let candidate_type = "struct";
        let passes = type_set.as_ref().map_or(true, |s| {
            s.iter().any(|t| candidate_type.eq_ignore_ascii_case(t))
        });
        assert!(passes);
    }

    #[test]
    fn test_lang_filter_empty_rejects_all() {
        // Empty language list means nothing passes
        let lang_set: Option<HashSet<String>> = Some(HashSet::new());
        let passes = lang_set
            .as_ref()
            .map_or(true, |s| s.iter().any(|l| "rust".eq_ignore_ascii_case(l)));
        assert!(!passes);
    }
}
