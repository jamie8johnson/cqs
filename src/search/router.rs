//! Query classifier and adaptive search strategy router.
//!
//! Classifies incoming queries by intent (identifier lookup, structural search,
//! behavioral search, etc.) and routes to the best retrieval strategy.
//! Pure logic — no I/O, no store access, infallible.

use crate::language::{ChunkType, REGISTRY};

/// Query categories for adaptive routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryCategory {
    /// Looking for a specific function/type by name ("search_filtered", "HashMap::new")
    IdentifierLookup,
    /// Searching for code by structure ("functions that return Result", "structs with Display")
    Structural,
    /// Searching for code by behavior ("validates user input", "retries with backoff")
    Behavioral,
    /// Searching for abstract concepts ("dependency injection", "observer pattern")
    Conceptual,
    /// Queries requiring multiple signals ("find where errors are logged and retried")
    MultiStep,
    /// Queries with negation ("sort without allocating", "parse but not validate")
    Negation,
    /// Queries constrained by chunk type ("all test functions", "every enum")
    TypeFiltered,
    /// Queries mentioning multiple languages ("Python equivalent of map in Rust")
    CrossLanguage,
    /// No clear category — use default strategy
    Unknown,
}

impl std::fmt::Display for QueryCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IdentifierLookup => write!(f, "identifier_lookup"),
            Self::Structural => write!(f, "structural"),
            Self::Behavioral => write!(f, "behavioral"),
            Self::Conceptual => write!(f, "conceptual"),
            Self::MultiStep => write!(f, "multi_step"),
            Self::Negation => write!(f, "negation"),
            Self::TypeFiltered => write!(f, "type_filtered"),
            Self::CrossLanguage => write!(f, "cross_language"),
            Self::Unknown => write!(f, "unknown"),
        }
    }
}

/// Classifier confidence level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    /// Strong signal — single strategy is optimal
    High,
    /// Mixed signals — may benefit from ensemble
    Medium,
    /// No clear signal — use default
    Low,
}

impl std::fmt::Display for Confidence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::High => write!(f, "high"),
            Self::Medium => write!(f, "medium"),
            Self::Low => write!(f, "low"),
        }
    }
}

/// Search strategy to use for a query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchStrategy {
    /// FTS5 name search — skip embedding entirely (~1ms)
    NameOnly,
    /// Standard dense embedding search (current default path, enriched HNSW)
    DenseDefault,
    /// Dense search with type boost for matching chunk types (enriched HNSW)
    DenseWithTypeHints,
    /// Phase 5: dense search against the base (non-enriched) HNSW — LLM
    /// summaries tend to hurt conceptual/behavioral/negation signal because
    /// they inject canonical vocabulary that drowns out query semantics.
    /// Falls back to [`Self::DenseDefault`] when the base index is missing.
    DenseBase,
}

impl std::fmt::Display for SearchStrategy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NameOnly => write!(f, "name_only"),
            Self::DenseDefault => write!(f, "dense"),
            Self::DenseWithTypeHints => write!(f, "dense_type_hints"),
            Self::DenseBase => write!(f, "dense_base"),
        }
    }
}

/// Classification result from the query router.
#[derive(Debug, Clone)]
pub struct Classification {
    pub category: QueryCategory,
    pub confidence: Confidence,
    pub strategy: SearchStrategy,
    /// Extracted type hints for DenseWithTypeHints strategy
    pub type_hints: Option<Vec<ChunkType>>,
}

// ── Common word lists ────────────────────────────────────────────────

/// Words that indicate natural language (not an identifier)
const NL_INDICATORS: &[&str] = &[
    "the",
    "a",
    "an",
    "that",
    "which",
    "how",
    "what",
    "where",
    "when",
    "find",
    "get",
    "all",
    "every",
    "each",
    "with",
    "without",
    "for",
    "from",
    "into",
    "this",
    "does",
    "code",
    "function",
    "method",
    "implement",
    "using",
];

/// Behavioral verbs suggesting a behavioral search
const BEHAVIORAL_VERBS: &[&str] = &[
    "validates",
    "processes",
    "handles",
    "manages",
    "computes",
    "parses",
    "converts",
    "transforms",
    "filters",
    "sorts",
    "checks",
    "verifies",
    "sends",
    "receives",
    "reads",
    "writes",
    "creates",
    "deletes",
    "updates",
    "serializes",
    "deserializes",
    "encodes",
    "decodes",
    "authenticates",
    "authorizes",
    "logs",
    "retries",
    "caches",
    "renders",
];

/// Abstract nouns suggesting conceptual search
const CONCEPTUAL_NOUNS: &[&str] = &[
    "pattern",
    "architecture",
    "design",
    "approach",
    "strategy",
    "algorithm",
    "principle",
    "abstraction",
    "convention",
    "idiom",
    "paradigm",
    "concept",
    "technique",
    "methodology",
];

/// Negation tokens matched against word-split query tokens (not substrings).
///
/// v1.22.0 audit AC-2: the previous pattern used trailing-space substring
/// matching (`query.contains("not ")`) which false-fired on words like
/// `cannot`, `piano`, `nano`, `volcano`, `casino`. Switched to exact
/// word-token matching against the `words` vec already computed upstream.
const NEGATION_TOKENS: &[&str] = &[
    "not",
    "without",
    "except",
    "never",
    "avoid",
    "no",
    "don't",
    "doesn't",
    "shouldn't",
    "exclude",
];

/// Structural keywords from programming languages
const STRUCTURAL_KEYWORDS: &[&str] = &[
    "struct",
    "enum",
    "trait",
    "impl",
    "interface",
    "class",
    "module",
    "namespace",
    "protocol",
    "type",
];

/// Common aliases that users type but don't match registry names.
/// Registry names cover the canonical forms ("cpp", "csharp", etc.);
/// these add the human-written variants.
const LANGUAGE_ALIASES: &[&str] = &["c++", "c#"];

/// Build the set of language names for cross-language detection.
///
/// Combines all registered language names from `REGISTRY.all()` with
/// common aliases that don't appear as registry keys.
fn language_names() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = REGISTRY.all().map(|def| def.name).collect();
    for alias in LANGUAGE_ALIASES {
        if !names.contains(alias) {
            names.push(alias);
        }
    }
    names
}

/// Structural query patterns
const STRUCTURAL_PATTERNS: &[&str] = &[
    "functions that",
    "methods that",
    "types that",
    "structs that",
    "that return",
    "that take",
    "that accept",
    "with signature",
    "implementing",
    "extending",
    "deriving",
];

/// Multi-step conjunction patterns
const MULTISTEP_PATTERNS: &[&str] = &[
    "and then", "before ", "after ", " and ", " or ", "first ", "then ", "both ", "between ",
];

// ── Classification ───────────────────────────────────────────────────

/// Classify a query into a category with confidence level and recommended strategy.
///
/// Resolve the SPLADE fusion alpha for a query category.
///
/// Precedence: per-category env (`CQS_SPLADE_ALPHA_{CATEGORY}`) > global env
/// (`CQS_SPLADE_ALPHA`) > hardcoded default (1.0 = pure dense, SPLADE off).
///
/// Returns a value in [0.0, 1.0] where 1.0 means pure dense and < 1.0 activates
/// SPLADE with that fusion weight.
pub fn resolve_splade_alpha(category: &QueryCategory) -> f32 {
    // Per-category env override: CQS_SPLADE_ALPHA_CONCEPTUAL_SEARCH etc.
    let cat_key = format!("CQS_SPLADE_ALPHA_{}", category.to_string().to_uppercase());
    if let Ok(val) = std::env::var(&cat_key) {
        if let Ok(alpha) = val.parse::<f32>() {
            if alpha.is_finite() {
                return alpha.clamp(0.0, 1.0);
            }
            tracing::warn!(var = %cat_key, value = %val, "Non-finite alpha, using default");
        } else {
            tracing::warn!(var = %cat_key, value = %val, "Invalid alpha, using default");
        }
    }

    // Global env override: CQS_SPLADE_ALPHA
    if let Ok(val) = std::env::var("CQS_SPLADE_ALPHA") {
        if let Ok(alpha) = val.parse::<f32>() {
            if alpha.is_finite() {
                return alpha.clamp(0.0, 1.0);
            }
        }
    }

    // Per-category defaults from 11-point alpha sweep (2026-04-13).
    // 265 queries × 8 categories. Verified with single-category reruns.
    match category {
        QueryCategory::IdentifierLookup => 0.9, // +4.0pp (98.0% vs 94.0%)
        QueryCategory::Structural => 0.7,       // +14.8pp (66.7% vs 51.9%) — verified
        QueryCategory::Conceptual => 0.9,       // +8.4pp (41.7% vs 33.3%)
        QueryCategory::TypeFiltered => 0.9,     // +4.2pp (37.5% vs 33.3%)
        QueryCategory::Behavioral => 0.1,       // +6.8pp (31.8% vs 25.0%) — verified
        _ => 1.0,                               // multi_step, cross_language, negation, unknown
    }
}

/// Pure function — no I/O, cannot fail, completes in <1ms.
/// Priority order: Negation > Identifier > CrossLanguage > TypeFiltered >
/// Structural > Behavioral > Conceptual > MultiStep > Unknown.
pub fn classify_query(query: &str) -> Classification {
    let query_lower = query.to_lowercase();
    let words: Vec<&str> = query_lower.split_whitespace().collect();

    if words.is_empty() {
        return Classification {
            category: QueryCategory::Unknown,
            confidence: Confidence::Low,
            strategy: SearchStrategy::DenseDefault,
            type_hints: None,
        };
    }

    // 1. Negation trumps everything — "sort without allocating".
    //    Phase 5: enriched summaries inject positive vocabulary ("allocates",
    //    "uses heap") that fights the negation, so route to the base index.
    if words.iter().any(|w| NEGATION_TOKENS.contains(w)) {
        return Classification {
            category: QueryCategory::Negation,
            confidence: Confidence::High,
            strategy: SearchStrategy::DenseBase,
            type_hints: None,
        };
    }

    // 2. Identifier lookup — all tokens look like identifiers
    if is_identifier_query(&query_lower, &words) {
        return Classification {
            category: QueryCategory::IdentifierLookup,
            confidence: Confidence::High,
            strategy: SearchStrategy::NameOnly,
            type_hints: None,
        };
    }

    // 3. Cross-language — mentions 2+ language names or "equivalent"/"translate".
    //    These benefit from the enriched index (summaries add canonical
    //    vocabulary that bridges language-specific syntax).
    if is_cross_language_query(&query_lower, &words) {
        return Classification {
            category: QueryCategory::CrossLanguage,
            confidence: Confidence::High,
            strategy: SearchStrategy::DenseDefault,
            type_hints: None,
        };
    }

    // 4. Type-filtered — "all structs", "every enum", "test functions"
    let type_hints = extract_type_hints(&query_lower);
    if type_hints.is_some() {
        return Classification {
            category: QueryCategory::TypeFiltered,
            confidence: Confidence::Medium,
            strategy: SearchStrategy::DenseWithTypeHints,
            type_hints,
        };
    }

    // 5. Structural — type keywords + "functions that" patterns
    if is_structural_query(&query_lower) {
        return Classification {
            category: QueryCategory::Structural,
            confidence: Confidence::Medium,
            strategy: SearchStrategy::DenseWithTypeHints,
            type_hints: None,
        };
    }

    // 6. Behavioral — action verbs, "code that does X".
    //    Phase 5: behavioral queries use verbs the query author chose; enriched
    //    summaries standardize those verbs ("handles" → "processes"), which
    //    washes out the specific verb the user asked about. Route to base.
    //
    //    2026-04-10 update: same-corpus A/B at 50% summary coverage shows
    //    behavioral routing produces 0pp delta — the routing fires but the
    //    affected queries' gold answers are mostly callable types where
    //    base ≈ enriched after enrichment_hash dedupe. Keeping the route on
    //    base because the historical research data still says behavioral
    //    is hurt by summaries; we just can't measure the effect on this
    //    corpus shape. See research/enrichment.md for the data.
    if is_behavioral_query(&query_lower, &words) {
        return Classification {
            category: QueryCategory::Behavioral,
            confidence: Confidence::Medium,
            strategy: SearchStrategy::DenseBase,
            type_hints: None,
        };
    }

    // 7. Conceptual — abstract nouns, short non-identifier queries.
    //
    //    2026-04-10 update: ROUTING REVERSED. Phase 5 originally routed
    //    conceptual to DenseBase based on the historical research finding
    //    "summaries hurt conceptual −15pp". That finding was measured on a
    //    corpus where only callable types were summarized.
    //
    //    After the eligibility expansion in PR #878 (summaries now cover
    //    structs / enums / impls / traits / classes / etc.), conceptual
    //    queries' gold answers are mostly type definitions where the
    //    summary actively helps bridge code → concept ("a service container
    //    that resolves dependencies" → "dependency injection"). Routing
    //    those queries to the base index strips the helpful signal.
    //
    //    Same-corpus A/B at 50% coverage measured −3.7pp R@1 on conceptual
    //    when routing was on. Keeping conceptual on the enriched index
    //    until / unless the summary coverage shape changes again.
    //
    //    The lesson: routing rules are coupled to corpus shape, not to
    //    a category-intrinsic property. They need to be re-validated any
    //    time summary coverage changes meaningfully.
    if is_conceptual_query(&query_lower, &words) {
        return Classification {
            category: QueryCategory::Conceptual,
            confidence: Confidence::Medium,
            strategy: SearchStrategy::DenseDefault,
            type_hints: None,
        };
    }

    // 8. Multi-step — conjunctions
    if MULTISTEP_PATTERNS.iter().any(|p| query_lower.contains(p)) {
        return Classification {
            category: QueryCategory::MultiStep,
            confidence: Confidence::Low,
            strategy: SearchStrategy::DenseDefault,
            type_hints: None,
        };
    }

    // 9. Unknown — default
    Classification {
        category: QueryCategory::Unknown,
        confidence: Confidence::Low,
        strategy: SearchStrategy::DenseDefault,
        type_hints: None,
    }
}

// ── Helpers ──────────────────────────────────────────────────────────

/// Check if a query looks like an identifier lookup.
/// All tokens must be valid identifier characters (a-z, 0-9, _, :, .)
/// and no natural language indicator words.
fn is_identifier_query(_query: &str, words: &[&str]) -> bool {
    // Single-word queries with identifier chars
    if words.len() == 1 {
        let w = words[0];
        // Must contain at least one letter
        if !w.chars().any(|c| c.is_alphabetic()) {
            return false;
        }
        // NL indicator words are not identifiers
        if NL_INDICATORS.contains(&w) {
            return false;
        }
        // Pure identifier chars (including :: and .)
        return w
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == ':' || c == '.' || c == '/');
    }

    // Multi-word: require at least one strong identifier signal
    // (underscore, ::, ., or mixed case within a single token)
    if words.len() <= 3 {
        let has_nl = words.iter().any(|w| NL_INDICATORS.contains(w));
        if has_nl {
            return false;
        }
        let has_identifier_signal = words.iter().any(|w| {
            w.contains('_')
                || w.contains("::")
                || w.contains('.')
                || (w.chars().any(|c| c.is_uppercase()) && w.chars().any(|c| c.is_lowercase()))
        });
        let all_identifier_chars = words.iter().all(|w| {
            w.chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == ':' || c == '.')
        });
        return has_identifier_signal && all_identifier_chars;
    }

    false
}

/// Check if query mentions multiple programming languages or translation.
fn is_cross_language_query(query: &str, words: &[&str]) -> bool {
    let names = language_names();
    let lang_count = names
        .iter()
        .filter(|l| words.iter().any(|w| *w == **l))
        .count();
    if lang_count >= 2 {
        return true;
    }
    if lang_count >= 1
        && (query.contains("equivalent")
            || query.contains("translate")
            || query.contains("port ")
            || query.contains("convert "))
    {
        return true;
    }
    false
}

/// Check if query is structural (about code structure, not behavior).
fn is_structural_query(query: &str) -> bool {
    // Structural patterns like "functions that return"
    if STRUCTURAL_PATTERNS.iter().any(|p| query.contains(p)) {
        return true;
    }
    // Contains structural keywords as NL words (not identifiers)
    // e.g., "find all structs" but not "MyStruct"
    STRUCTURAL_KEYWORDS
        .iter()
        .any(|kw| query.contains(&format!(" {} ", kw)) || query.starts_with(&format!("{} ", kw)))
}

/// Check if query describes behavior.
fn is_behavioral_query(query: &str, words: &[&str]) -> bool {
    if words.iter().any(|w| BEHAVIORAL_VERBS.contains(w)) {
        return true;
    }
    query.contains("how does")
        || query.contains("what does")
        || query.contains("code that")
        || query.contains("function that")
}

/// Check if query is about abstract concepts.
fn is_conceptual_query(query: &str, words: &[&str]) -> bool {
    if words.iter().any(|w| CONCEPTUAL_NOUNS.contains(w)) {
        return true;
    }
    // Short queries (1-3 words) that aren't identifiers and aren't structural
    words.len() <= 3
        && words.iter().any(|w| NL_INDICATORS.contains(w))
        && !is_structural_query(query)
}

/// Extract chunk type hints from the query text.
///
/// Returns the types to boost (not filter) in search results.
/// Only extracts when confidence is reasonable — avoids false positives.
pub fn extract_type_hints(query: &str) -> Option<Vec<ChunkType>> {
    let mut types = Vec::new();

    let patterns: &[(&str, ChunkType)] = &[
        // Test
        ("test function", ChunkType::Test),
        ("test method", ChunkType::Test),
        ("all tests", ChunkType::Test),
        ("every test", ChunkType::Test),
        // Function / Method
        ("all functions", ChunkType::Function),
        ("every function", ChunkType::Function),
        ("all methods", ChunkType::Method),
        ("every method", ChunkType::Method),
        // Type definitions
        ("all structs", ChunkType::Struct),
        ("every struct", ChunkType::Struct),
        ("all enums", ChunkType::Enum),
        ("every enum", ChunkType::Enum),
        ("all traits", ChunkType::Trait),
        ("every trait", ChunkType::Trait),
        ("all interfaces", ChunkType::Interface),
        ("every interface", ChunkType::Interface),
        ("all classes", ChunkType::Class),
        ("every class", ChunkType::Class),
        ("type alias", ChunkType::TypeAlias),
        ("all type aliases", ChunkType::TypeAlias),
        // OOP / module constructs
        ("all modules", ChunkType::Module),
        ("every module", ChunkType::Module),
        ("all objects", ChunkType::Object),
        ("every object", ChunkType::Object),
        ("all namespaces", ChunkType::Namespace),
        ("every namespace", ChunkType::Namespace),
        ("all impl blocks", ChunkType::Impl),
        ("implementation block", ChunkType::Impl),
        ("extension method", ChunkType::Extension),
        ("all extensions", ChunkType::Extension),
        // Members
        ("all constants", ChunkType::Constant),
        ("every constant", ChunkType::Constant),
        ("all variables", ChunkType::Variable),
        ("every variable", ChunkType::Variable),
        ("all properties", ChunkType::Property),
        ("every property", ChunkType::Property),
        ("constructor", ChunkType::Constructor),
        ("all constructors", ChunkType::Constructor),
        // C# specific
        ("all delegates", ChunkType::Delegate),
        ("every delegate", ChunkType::Delegate),
        ("all events", ChunkType::Event),
        ("every event", ChunkType::Event),
        // Macros
        ("all macros", ChunkType::Macro),
        ("every macro", ChunkType::Macro),
        ("macro_rules", ChunkType::Macro),
        // Web / API
        ("endpoint", ChunkType::Endpoint),
        ("all endpoints", ChunkType::Endpoint),
        ("all services", ChunkType::Service),
        ("every service", ChunkType::Service),
        ("middleware", ChunkType::Middleware),
        ("all middleware", ChunkType::Middleware),
        // Database / FFI / config
        ("stored procedure", ChunkType::StoredProc),
        ("all stored procedures", ChunkType::StoredProc),
        ("extern function", ChunkType::Extern),
        ("all externs", ChunkType::Extern),
        ("ffi declaration", ChunkType::Extern),
        ("config key", ChunkType::ConfigKey),
        ("all config keys", ChunkType::ConfigKey),
        // Docs / Solidity
        ("all sections", ChunkType::Section),
        ("every section", ChunkType::Section),
        ("all modifiers", ChunkType::Modifier),
        ("every modifier", ChunkType::Modifier),
    ];

    for (pattern, chunk_type) in patterns {
        if query.contains(pattern) {
            types.push(*chunk_type);
        }
    }

    if types.is_empty() {
        None
    } else {
        Some(types)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Happy path (13 tests) ────────────────────────────────────────

    #[test]
    fn test_classify_identifier_snake_case() {
        let c = classify_query("search_filtered");
        assert_eq!(c.category, QueryCategory::IdentifierLookup);
        assert_eq!(c.confidence, Confidence::High);
        assert_eq!(c.strategy, SearchStrategy::NameOnly);
    }

    #[test]
    fn test_classify_identifier_qualified() {
        let c = classify_query("HashMap::new");
        assert_eq!(c.category, QueryCategory::IdentifierLookup);
        assert_eq!(c.confidence, Confidence::High);
    }

    #[test]
    fn test_classify_identifier_camel() {
        let c = classify_query("SearchFilter");
        assert_eq!(c.category, QueryCategory::IdentifierLookup);
        assert_eq!(c.confidence, Confidence::High);
    }

    #[test]
    fn test_classify_behavioral() {
        let c = classify_query("validates user input");
        assert_eq!(c.category, QueryCategory::Behavioral);
        assert_eq!(c.confidence, Confidence::Medium);
        // Phase 5: behavioral routes to the base (non-enriched) index because
        // LLM summaries flatten the specific verbs users ask about.
        assert_eq!(c.strategy, SearchStrategy::DenseBase);
    }

    #[test]
    fn test_classify_negation() {
        let c = classify_query("sort without allocating");
        assert_eq!(c.category, QueryCategory::Negation);
        assert_eq!(c.confidence, Confidence::High);
        // Phase 5: negation routes to base — summaries inject positive
        // vocabulary that fights the "without" clause.
        assert_eq!(c.strategy, SearchStrategy::DenseBase);
    }

    #[test]
    fn test_classify_conceptual_routes_to_enriched() {
        // 2026-04-10: ROUTING REVERSED. Originally Phase 5 routed conceptual
        // to DenseBase based on the historical research finding "summaries
        // hurt conceptual −15pp". Same-corpus A/B at 50% summary coverage
        // measured −3.7pp R@1 from that routing — the historical finding
        // was for a different corpus shape (only callables summarized).
        // After PR #878 expanded summaries to type definitions, conceptual
        // queries' gold answers benefit from the enrichment (the summary
        // bridges code → concept on struct/enum chunks).
        //
        // See research/enrichment.md "Same-corpus A/B/C/D matrix (50% coverage)"
        // for the data that drove this revision.
        let c = classify_query("dependency injection pattern");
        assert_eq!(c.category, QueryCategory::Conceptual);
        assert_eq!(c.strategy, SearchStrategy::DenseDefault);
    }

    #[test]
    fn test_classify_structural_stays_on_enriched() {
        // Phase 5 regression: structural queries benefit from enrichment,
        // so they keep the DenseWithTypeHints (enriched HNSW) strategy.
        let c = classify_query("functions that return Result");
        assert_eq!(c.category, QueryCategory::Structural);
        assert_eq!(c.strategy, SearchStrategy::DenseWithTypeHints);
    }

    #[test]
    fn test_classify_cross_language_stays_on_enriched() {
        // Phase 5 regression: cross-language queries rely on canonical
        // vocabulary that summaries provide, so they stay on enriched.
        let c = classify_query("Python equivalent of map in Rust");
        assert_eq!(c.category, QueryCategory::CrossLanguage);
        assert_eq!(c.strategy, SearchStrategy::DenseDefault);
    }

    #[test]
    fn test_classify_structural() {
        let c = classify_query("functions that return Result");
        assert_eq!(c.category, QueryCategory::Structural);
        assert_eq!(c.confidence, Confidence::Medium);
    }

    #[test]
    fn test_classify_type_filtered() {
        let c = classify_query("all test functions");
        assert_eq!(c.category, QueryCategory::TypeFiltered);
        assert!(c.type_hints.is_some());
        assert!(c.type_hints.unwrap().contains(&ChunkType::Test));
    }

    #[test]
    fn test_classify_cross_language() {
        let c = classify_query("Python equivalent of map in Rust");
        assert_eq!(c.category, QueryCategory::CrossLanguage);
        assert_eq!(c.confidence, Confidence::High);
    }

    #[test]
    fn test_classify_conceptual() {
        let c = classify_query("dependency injection pattern");
        assert_eq!(c.category, QueryCategory::Conceptual);
        assert_eq!(c.confidence, Confidence::Medium);
    }

    #[test]
    fn test_classify_multi_step() {
        let c = classify_query("find errors and then retry them");
        assert_eq!(c.category, QueryCategory::MultiStep);
        assert_eq!(c.confidence, Confidence::Low);
    }

    #[test]
    fn test_classify_unknown() {
        let c = classify_query("asdf jkl qwerty");
        assert_eq!(c.category, QueryCategory::Unknown);
        assert_eq!(c.confidence, Confidence::Low);
    }

    #[test]
    fn test_extract_type_hints_struct() {
        let hints = extract_type_hints("find all structs");
        assert!(hints.is_some());
        assert!(hints.unwrap().contains(&ChunkType::Struct));
    }

    #[test]
    fn test_extract_type_hints_none() {
        let hints = extract_type_hints("handle errors gracefully");
        assert!(hints.is_none());
    }

    // ── Adversarial (15 tests) ───────────────────────────────────────

    #[test]
    fn test_classify_empty() {
        let c = classify_query("");
        assert_eq!(c.category, QueryCategory::Unknown);
        assert_eq!(c.confidence, Confidence::Low);
    }

    #[test]
    fn test_classify_single_char() {
        let c = classify_query("a");
        // "a" is an NL indicator, not an identifier
        assert_ne!(c.category, QueryCategory::IdentifierLookup);
    }

    #[test]
    fn test_classify_very_long() {
        let long = "a ".repeat(5000);
        let start = std::time::Instant::now();
        let c = classify_query(&long);
        let elapsed = start.elapsed();
        assert!(elapsed.as_millis() < 100, "Should complete in <100ms");
        assert_eq!(c.confidence, Confidence::Low);
    }

    #[test]
    fn test_classify_unicode_identifier() {
        let c = classify_query("日本語_関数");
        assert_eq!(c.category, QueryCategory::IdentifierLookup);
    }

    #[test]
    fn test_classify_path_like() {
        let c = classify_query("src/store/mod.rs");
        assert_eq!(c.category, QueryCategory::IdentifierLookup);
    }

    #[test]
    fn test_classify_only_stopwords() {
        let c = classify_query("the a an of");
        assert_ne!(c.category, QueryCategory::IdentifierLookup);
    }

    #[test]
    fn test_classify_special_chars() {
        let c = classify_query("fn<T: Hash>()");
        // Contains "fn" which triggers structural, but also looks like code
        // Key: doesn't panic
        let _ = c;
    }

    #[test]
    fn test_classify_all_caps() {
        let c = classify_query("WHERE IS THE ERROR HANDLER");
        // Contains NL words, should not be identifier
        assert_ne!(c.category, QueryCategory::IdentifierLookup);
    }

    #[test]
    fn test_classify_numbers() {
        let c = classify_query("404");
        // Pure number — has no alphabetic chars
        assert_eq!(c.category, QueryCategory::Unknown);
    }

    #[test]
    fn test_classify_hex() {
        let c = classify_query("0xFF");
        // Starts with digit, has alpha — could be identifier
        assert_eq!(c.category, QueryCategory::IdentifierLookup);
    }

    #[test]
    fn test_classify_mixed_signals() {
        let c = classify_query("not struct");
        // Negation trumps structural
        assert_eq!(c.category, QueryCategory::Negation);
    }

    #[test]
    fn test_classify_sql_injection() {
        let c = classify_query("'; DROP TABLE--");
        // Should not panic, should not be identifier
        assert_ne!(c.category, QueryCategory::IdentifierLookup);
    }

    #[test]
    fn test_classify_null_bytes() {
        let c = classify_query("foo\0bar");
        // Should handle gracefully — no panic
        let _ = c;
    }

    #[test]
    fn test_classify_type_hint_wrong_extraction() {
        // "error handling" should NOT extract Enum type hint
        // even though "error" could be confused with an error enum
        let hints = extract_type_hints("error handling");
        assert!(hints.is_none());
    }

    #[test]
    fn test_classify_identifier_common_word() {
        // "error" alone is ambiguous — could be identifier or concept
        let c = classify_query("error");
        // Should be identifier (single word, valid identifier chars)
        // but Medium confidence since it's also a common word
        assert_eq!(c.category, QueryCategory::IdentifierLookup);
    }
}
