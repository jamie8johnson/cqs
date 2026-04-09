//! Search filter and scoring options.

use crate::parser::{ChunkType, Language};

/// Default name_boost weight used by CLI commands (0.2 = 20% name match influence).
/// The struct default is 0.0 (no name boost) for API callers; CLI applies this.
pub const DEFAULT_NAME_BOOST: f32 = 0.2;

/// Filter and scoring options for search.
///
/// Fields are public for direct construction via struct literals.
/// [`SearchFilter::with_query()`] is a convenience builder for setting query text.
///
/// All fields are optional. Unset filters match all chunks.
/// Use [`SearchFilter::validate()`] to check constraints before searching.
pub struct SearchFilter {
    /// Filter by programming language(s)
    pub languages: Option<Vec<Language>>,
    /// Include only these chunk types (function, method, class, struct, test, endpoint, etc.)
    pub include_types: Option<Vec<ChunkType>>,
    /// Exclude these chunk types from results (e.g., test, variable, configkey)
    pub exclude_types: Option<Vec<ChunkType>>,
    /// Filter by file path glob pattern (e.g., `src/**/*.rs`)
    pub path_pattern: Option<String>,
    /// Weight for name matching in hybrid search (0.0-1.0)
    ///
    /// 0.0 = pure embedding similarity (default)
    /// 1.0 = pure name matching
    /// 0.2 = recommended for balanced results
    pub name_boost: f32,
    /// Query text for name matching (required if name_boost > 0 or enable_rrf)
    pub query_text: String,
    /// Enable RRF (Reciprocal Rank Fusion) hybrid search
    ///
    /// When enabled, combines semantic search results with FTS5 keyword search
    /// using the formula: score = Σ 1/(k + rank), where k=60.
    /// This typically improves recall for identifier-heavy queries.
    pub enable_rrf: bool,
    /// Apply search-time demotion for test functions and underscore-prefixed names.
    ///
    /// Test functions (`test_*`, `Test*`) get 0.90x multiplier.
    /// Underscore-prefixed private names (`_foo` but not `__dunder__`) get 0.95x.
    /// Disable with `--no-demote` CLI flag.
    pub enable_demotion: bool,
    /// Enable SPLADE sparse-dense hybrid search.
    ///
    /// When enabled, queries are encoded with both the dense embedder and the
    /// SPLADE sparse encoder. Results are fused via linear interpolation.
    pub enable_splade: bool,
    /// SPLADE fusion weight: 1.0 = pure cosine, 0.0 = pure sparse.
    /// Only used when enable_splade is true.
    pub splade_alpha: f32,
    /// Chunk types to boost in scoring (from adaptive routing).
    ///
    /// When set, results matching these types get a 1.2x score multiplier.
    /// This is additive (boost), not restrictive (filter) — non-matching
    /// types still appear, just ranked slightly lower.
    pub type_boost_types: Option<Vec<ChunkType>>,
}

impl Default for SearchFilter {
    fn default() -> Self {
        Self {
            languages: None,
            include_types: None,
            exclude_types: None,
            path_pattern: None,
            name_boost: 0.0,
            query_text: String::new(),
            enable_rrf: false,
            enable_demotion: true, // Demote test functions by default
            enable_splade: false,
            splade_alpha: 0.7,
            type_boost_types: None,
        }
    }
}

impl SearchFilter {
    /// Set the query text (required for name_boost > 0 or enable_rrf).
    pub fn with_query(mut self, query: impl Into<String>) -> Self {
        self.query_text = query.into();
        self
    }

    /// Validate filter constraints
    ///
    /// Returns Ok(()) if valid, or Err with description of what's wrong.
    pub fn validate(&self) -> Result<(), String> {
        // name_boost must be in [0.0, 1.0] (NaN-safe: NaN is not contained in any range)
        if !(0.0..=1.0).contains(&self.name_boost) {
            return Err(format!(
                "name_boost must be between 0.0 and 1.0, got {}",
                self.name_boost
            ));
        }

        // query_text required when name_boost > 0 or enable_rrf
        if (self.name_boost > 0.0 || self.enable_rrf) && self.query_text.is_empty() {
            return Err(
                "query_text required when name_boost > 0 or enable_rrf is true".to_string(),
            );
        }

        // path_pattern must be valid glob syntax if provided
        if let Some(ref pattern) = self.path_pattern {
            if pattern.len() > 500 {
                return Err("path_pattern too long (max 500 chars)".to_string());
            }
            // Reject control characters (except tab/newline which glob might handle)
            if pattern
                .chars()
                .any(|c| c.is_control() && c != '\t' && c != '\n')
            {
                return Err("path_pattern contains invalid control characters".to_string());
            }
            // Limit brace nesting depth to prevent exponential expansion
            // e.g., "{a,{b,{c,{d,{e,...}}}}}" can cause O(2^n) expansion
            const MAX_BRACE_DEPTH: usize = 10;
            let mut depth = 0usize;
            for c in pattern.chars() {
                match c {
                    '{' => {
                        depth += 1;
                        if depth > MAX_BRACE_DEPTH {
                            return Err("path_pattern has too many nested braces (max 10 levels)"
                                .to_string());
                        }
                    }
                    '}' => depth = depth.saturating_sub(1),
                    _ => {}
                }
            }
            if globset::Glob::new(pattern).is_err() {
                return Err("path_pattern is not a valid glob pattern".to_string());
            }
        }

        // splade_alpha must be in [0.0, 1.0] when SPLADE is enabled (RB-12)
        if self.enable_splade && !(0.0..=1.0).contains(&self.splade_alpha) {
            return Err(format!(
                "splade_alpha must be between 0.0 and 1.0, got {}",
                self.splade_alpha
            ));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_search_filter_valid_default() {
        let filter = SearchFilter::default();
        assert!(filter.validate().is_ok());
    }

    #[test]
    fn test_search_filter_valid_with_name_boost() {
        let filter = SearchFilter {
            name_boost: 0.2,
            query_text: "test".to_string(),
            ..Default::default()
        };
        assert!(filter.validate().is_ok());
    }

    #[test]
    fn test_search_filter_valid_with_rrf() {
        let filter = SearchFilter {
            enable_rrf: true,
            query_text: "test".to_string(),
            ..Default::default()
        };
        assert!(filter.validate().is_ok());
    }

    #[test]
    fn test_search_filter_invalid_name_boost_negative() {
        let filter = SearchFilter {
            name_boost: -0.1,
            ..Default::default()
        };
        assert!(filter.validate().is_err());
        assert!(filter.validate().unwrap_err().contains("name_boost"));
    }

    #[test]
    fn test_search_filter_invalid_name_boost_nan() {
        let filter = SearchFilter {
            name_boost: f32::NAN,
            ..Default::default()
        };
        assert!(filter.validate().is_err());
        assert!(filter.validate().unwrap_err().contains("name_boost"));
    }

    #[test]
    fn test_search_filter_invalid_name_boost_too_high() {
        let filter = SearchFilter {
            name_boost: 1.5,
            query_text: "test".to_string(),
            ..Default::default()
        };
        assert!(filter.validate().is_err());
    }

    #[test]
    fn test_search_filter_invalid_missing_query_text() {
        let filter = SearchFilter {
            name_boost: 0.5,
            query_text: String::new(),
            ..Default::default()
        };
        assert!(filter.validate().is_err());
        assert!(filter.validate().unwrap_err().contains("query_text"));
    }

    #[test]
    fn test_search_filter_invalid_rrf_missing_query() {
        let filter = SearchFilter {
            enable_rrf: true,
            query_text: String::new(),
            ..Default::default()
        };
        assert!(filter.validate().is_err());
    }

    #[test]
    fn test_search_filter_valid_path_pattern() {
        let filter = SearchFilter {
            path_pattern: Some("src/**/*.rs".to_string()),
            ..Default::default()
        };
        assert!(filter.validate().is_ok());
    }

    #[test]
    fn test_search_filter_invalid_path_pattern_syntax() {
        let filter = SearchFilter {
            path_pattern: Some("[invalid".to_string()),
            ..Default::default()
        };
        assert!(filter.validate().is_err());
        assert!(filter.validate().unwrap_err().contains("glob"));
    }

    #[test]
    fn test_search_filter_path_pattern_too_long() {
        let filter = SearchFilter {
            path_pattern: Some("a".repeat(501)),
            ..Default::default()
        };
        assert!(filter.validate().is_err());
        assert!(filter.validate().unwrap_err().contains("too long"));
    }

    // TC-34: SPLADE NaN alpha guard
    #[test]
    fn tc34_splade_nan_alpha_rejected() {
        let filter = SearchFilter {
            enable_splade: true,
            splade_alpha: f32::NAN,
            ..Default::default()
        };
        let result = filter.validate();
        assert!(result.is_err(), "NaN splade_alpha should be rejected");
        assert!(
            result.unwrap_err().contains("splade_alpha"),
            "Error message should mention splade_alpha"
        );
    }

    #[test]
    fn tc34_splade_alpha_out_of_range_rejected() {
        let filter = SearchFilter {
            enable_splade: true,
            splade_alpha: 1.5,
            ..Default::default()
        };
        assert!(filter.validate().is_err());

        let filter2 = SearchFilter {
            enable_splade: true,
            splade_alpha: -0.1,
            ..Default::default()
        };
        assert!(filter2.validate().is_err());
    }

    #[test]
    fn tc34_splade_alpha_valid_when_disabled() {
        // NaN alpha should be ok when SPLADE is disabled (not validated)
        let filter = SearchFilter {
            enable_splade: false,
            splade_alpha: f32::NAN,
            ..Default::default()
        };
        assert!(filter.validate().is_ok());
    }
}
