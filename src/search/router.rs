//! Query classifier and adaptive search strategy router.
//!
//! Classifies incoming queries by intent (identifier lookup, structural search,
//! behavioral search, etc.) and routes to the best retrieval strategy.
//! Pure logic — no I/O, no store access, infallible.

use crate::language::{ChunkType, REGISTRY};
use aho_corasick::{AhoCorasick, AhoCorasickBuilder, MatchKind};
use std::collections::HashMap;
use std::sync::{LazyLock, OnceLock};

// ---------------------------------------------------------------------------
// Macro: define_query_categories!
//
// Generates from a single declaration table:
//   - `QueryCategory` enum with Debug, Clone, Copy, PartialEq, Eq, Hash
//   - `Display` impl (variant → snake_case name)
//   - `QueryCategory::from_snake_case` (snake_case name + optional aliases → variant)
//   - `QueryCategory::all_variants() -> &'static [QueryCategory]`
//   - `QueryCategory::default_alpha(&self) -> f32` — exhaustive, no catch-all
//
// Adding a category = one new line here. The match in `resolve_splade_alpha`
// no longer contains a `_ => 1.0` catch-all: a missing `default_alpha = ...`
// is a compile error, surfacing the SPLADE-tuning gap that previously could
// ship invisibly.
// ---------------------------------------------------------------------------
/// Generates a `QueryCategory` enum with associated trait implementations and SPLADE alpha defaults.
///
/// # Arguments
///
/// - `$variant`: Identifier for each enum variant
/// - `$doc`: Optional documentation comments for each variant
/// - `$name`: snake_case string literal (used by `Display` and `from_snake_case`)
/// - `$alpha`: f32 default SPLADE alpha for the variant (required, no catch-all)
/// - `$alias`: Optional snake_case aliases that also parse to the variant
///
/// # Returns
///
/// Expands to:
/// - A `QueryCategory` enum with all specified variants
/// - `Display` impl that maps variants to their snake_case names
/// - `from_snake_case` method that parses primary names and aliases into variants
/// - `all_variants()` method returning a slice of every category
/// - `default_alpha()` method returning the per-variant SPLADE alpha (exhaustive)
macro_rules! define_query_categories {
    (
        $(
            $(#[doc = $doc:expr])*
            $variant:ident => $name:literal, default_alpha = $alpha:expr
                $(, aliases = [ $($alias:literal),* $(,)? ])?
                ;
        )+
    ) => {
        /// Query categories for adaptive routing.
        ///
        /// `Serialize` emits the canonical snake_case `Display` name.
        /// `Deserialize` is implemented out-of-macro (immediately below) so
        /// it routes through `from_snake_case` and honors the alias table —
        /// required because the on-disk eval JSON carries both `"behavioral"`
        /// and the historical `"behavioral_search"` for the same variant.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum QueryCategory {
            $(
                $(#[doc = $doc])*
                $variant,
            )+
        }

        impl serde::Serialize for QueryCategory {
            fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
                ser.collect_str(self)
            }
        }

        impl<'de> serde::Deserialize<'de> for QueryCategory {
            fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
                let s = <std::borrow::Cow<'de, str> as serde::Deserialize<'de>>::deserialize(de)?;
                Self::from_snake_case(&s).ok_or_else(|| {
                    serde::de::Error::unknown_variant(
                        &s,
                        // Static slice of canonical names — kept in sync by the macro.
                        &[ $( $name ),+ ],
                    )
                })
            }
        }

        impl std::fmt::Display for QueryCategory {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                match self {
                    $( Self::$variant => write!(f, $name), )+
                }
            }
        }

        impl QueryCategory {
            /// Parse a snake_case category name (or one of its aliases) into a
            /// variant. Returns `None` for unknown strings.
            pub fn from_snake_case(s: &str) -> Option<Self> {
                match s {
                    $(
                        $name => Some(Self::$variant),
                        $( $( $alias => Some(Self::$variant), )* )?
                    )+
                    _ => None,
                }
            }

            /// Every declared variant, in declaration order.
            pub fn all_variants() -> &'static [QueryCategory] {
                &[ $( Self::$variant ),+ ]
            }

            /// Default SPLADE fusion alpha for this category.
            ///
            /// Sourced from per-category sweeps (see `resolve_splade_alpha`
            /// for the methodology and history). Exhaustive — adding a new
            /// variant without `default_alpha = ...` is a compile error.
            pub fn default_alpha(&self) -> f32 {
                match self {
                    $( Self::$variant => $alpha, )+
                }
            }
        }
    };
}

define_query_categories! {
    /// Looking for a specific function/type by name ("search_filtered", "HashMap::new")
    IdentifierLookup => "identifier_lookup", default_alpha = 1.00;
    /// Searching for code by structure ("functions that return Result", "structs with Display")
    Structural => "structural", default_alpha = 0.90, aliases = ["structural_search"];
    /// Searching for code by behavior ("validates user input", "retries with backoff").
    /// Bumped 0.00 → 0.80 in v1.28.3 — original sweep optimized R@1 (where pure
    /// SPLADE wins on action-verb exact matches), but R@5 wants heavy dense for
    /// broader candidate recall. v3.v2 sweep direction was consistent across
    /// train/test/dev (best ∈ [0.65, 0.90]). Production lift is bottlenecked
    /// by `behavioral` classifier accuracy (~19% fire rate per the v3 audit);
    /// only correctly-routed queries see the lift.
    Behavioral => "behavioral", default_alpha = 0.80, aliases = ["behavioral_search"];
    /// Searching for abstract concepts ("dependency injection", "observer pattern")
    Conceptual => "conceptual", default_alpha = 0.70, aliases = ["conceptual_search"];
    /// Queries requiring multiple signals ("find where errors are logged and retried").
    /// Dropped 1.00 → 0.10 in v1.28.3 — multi-clause queries have heavy keyword
    /// overlap that SPLADE catches well at depth; pure dense was optimizing R@1
    /// at the cost of R@5. v3.v2 sweep direction consistent across train/test/dev
    /// (best ∈ [0.05, 0.10]). Same classifier-accuracy caveat as `behavioral`:
    /// the rule-based classifier rarely fires `multi_step` correctly because
    /// "X AND Y" patterns trip the structural rule first.
    MultiStep => "multi_step", default_alpha = 0.10;
    /// Queries with negation ("sort without allocating", "parse but not validate")
    Negation => "negation", default_alpha = 0.80;
    /// Queries constrained by chunk type ("all test functions", "every enum")
    TypeFiltered => "type_filtered", default_alpha = 1.00;
    /// Queries mentioning multiple languages ("Python equivalent of map in Rust").
    /// Ships 2026-04-16: 1.00 → 0.10 based on v3 sweep.
    CrossLanguage => "cross_language", default_alpha = 0.10;
    /// No clear category — use default strategy
    Unknown => "unknown", default_alpha = 1.00;
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

/// Aho-Corasick automaton over [`NL_INDICATORS`] for whole-query scans.
/// Match ids are not used — only the presence of a whole-word match matters.
static NL_INDICATORS_AC: LazyLock<AhoCorasick> = LazyLock::new(|| {
    AhoCorasick::new(NL_INDICATORS).expect("NL_INDICATORS is a valid pattern set (static)")
});

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

/// Aho-Corasick automaton over [`BEHAVIORAL_VERBS`].
/// Only whole-word matches trigger behavioral classification.
static BEHAVIORAL_VERBS_AC: LazyLock<AhoCorasick> = LazyLock::new(|| {
    AhoCorasick::new(BEHAVIORAL_VERBS).expect("BEHAVIORAL_VERBS is a valid pattern set (static)")
});

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

/// Aho-Corasick automaton over [`CONCEPTUAL_NOUNS`].
/// Only whole-word matches trigger conceptual classification.
static CONCEPTUAL_NOUNS_AC: LazyLock<AhoCorasick> = LazyLock::new(|| {
    AhoCorasick::new(CONCEPTUAL_NOUNS).expect("CONCEPTUAL_NOUNS is a valid pattern set (static)")
});

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
///
/// Materialized once at first use — the registry is immutable and the
/// alias list is a compile-time constant, so every subsequent call
/// returns a borrow of the same `Vec`. Previously this allocated a new
/// `Vec<&'static str>` on every `classify_query` call.
static LANGUAGE_NAMES: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
    let mut names: Vec<&'static str> = REGISTRY.all().map(|def| def.name).collect();
    for alias in LANGUAGE_ALIASES {
        if !names.contains(alias) {
            names.push(alias);
        }
    }
    names
});

/// Return the cached language-name list as a borrowed slice.
fn language_names() -> &'static [&'static str] {
    LANGUAGE_NAMES.as_slice()
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

/// Aho-Corasick automaton over [`STRUCTURAL_PATTERNS`]. These are matched as
/// raw substrings in the query (same as the previous `query.contains(pat)`),
/// so any match — word-bounded or not — triggers structural classification.
static STRUCTURAL_PATTERNS_AC: LazyLock<AhoCorasick> = LazyLock::new(|| {
    AhoCorasick::new(STRUCTURAL_PATTERNS)
        .expect("STRUCTURAL_PATTERNS is a valid pattern set (static)")
});

/// Multi-step conjunction patterns.
///
/// AC-V1.25-10: bare " and " / " or " were removed because they fired on
/// any conjunction in a query ("find foo and bar"), sweeping near-every
/// multi-word NL query into `QueryCategory::MultiStep`. The remaining
/// patterns require explicit sequencing / enumeration phrasing
/// ("first do X then do Y") so the category actually captures multi-step
/// intent, not any coordinated phrase.
const MULTISTEP_PATTERNS: &[&str] = &[
    "and then",
    "before ",
    "after ",
    " or also ",
    "first ",
    "then ",
    "both ",
    "between ",
];

/// Aho-Corasick automaton over [`MULTISTEP_PATTERNS`]. Raw substring match —
/// the pattern strings already carry their own trailing / leading space
/// where word-boundary semantics are needed.
static MULTISTEP_PATTERNS_AC: LazyLock<AhoCorasick> = LazyLock::new(|| {
    AhoCorasick::new(MULTISTEP_PATTERNS)
        .expect("MULTISTEP_PATTERNS is a valid pattern set (static)")
});

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
///
/// OB-NEW-1: emits a single structured `tracing::info!` recording the
/// resolved alpha, its source (`per_cat_env` / `global_env` / `default`),
/// and the category. Callers no longer need to log the decision themselves;
/// rooting the log inside this function makes the env-precedence visible and
/// eliminates the drift that existed between the CLI and batch-handler logs.
pub fn resolve_splade_alpha(category: &QueryCategory) -> f32 {
    let _span = tracing::debug_span!("resolve_splade_alpha", category = %category).entered();

    // Per-category env override: CQS_SPLADE_ALPHA_CONCEPTUAL_SEARCH etc.
    //
    // Rust 1.95 if-let guards collapse the previous nested
    // `if let Ok(val) { if let Ok(alpha) { ... } else { warn } }` into a
    // single match: each Ok-arm carries the env value straight into its
    // tracing call without an extra `else` block. The happy-path arm keeps
    // the inner `if alpha.is_finite()` so the non-finite warning can re-use
    // the already-parsed `alpha` without a second `parse::<f32>()` round
    // (clippy::collapsible_match would inline the predicate at the cost of
    // re-parsing in the second arm — not worth it for a hot path).
    let cat_key = format!("CQS_SPLADE_ALPHA_{}", category.to_string().to_uppercase());
    #[allow(clippy::collapsible_match)]
    match std::env::var(&cat_key) {
        Ok(val) if let Ok(alpha) = val.parse::<f32>() => {
            if alpha.is_finite() {
                let alpha = alpha.clamp(0.0, 1.0);
                // OB-V1.30.1-1 / P3-OB-1: per-search routing fires on every
                // query — keep the entry `info_span!` for traceability and
                // demote the inner event to debug so the operator default
                // log level isn't flooded.
                tracing::debug!(
                    category = %category,
                    alpha,
                    source = "per_cat_env",
                    "SPLADE routing"
                );
                return alpha;
            }
            tracing::warn!(var = %cat_key, value = %val, "Non-finite alpha, using default");
        }
        Ok(val) => {
            tracing::warn!(var = %cat_key, value = %val, "Invalid alpha, using default");
        }
        Err(_) => {}
    }

    // Global env override: CQS_SPLADE_ALPHA
    #[allow(clippy::collapsible_match)]
    match std::env::var("CQS_SPLADE_ALPHA") {
        Ok(val) if let Ok(alpha) = val.parse::<f32>() => {
            if alpha.is_finite() {
                let alpha = alpha.clamp(0.0, 1.0);
                // OB-V1.30.1-1 / P3-OB-1: see comment above on per-cat env.
                tracing::debug!(
                    category = %category,
                    alpha,
                    source = "global_env",
                    "SPLADE routing"
                );
                return alpha;
            }
        }
        _ => {}
    }

    // Per-category defaults from the 21-point alpha sweep on the genuinely
    // clean index (2026-04-15). 265 queries × 8 categories, 14,882 chunks
    // post-GC + worktree-duplicate purge.
    //
    // History: the 2026-04-14 "clean" sweep was actually run on a 96k-chunk
    // index polluted by auto-indexed `.claude/worktrees/` copies (daemon
    // watch ignored .gitignore, fixed in #1003). The dirty-tuned alphas
    // drove SPLADE-enabled R@1 to 26.8% (vs 35.8% dense-only) until
    // re-measured. The values here reflect the real clean-index optima —
    // overall R@1 41% projected (vs 37.7% for global α=0.90).
    //
    // Run artifacts: /home/user001/.cache/cqs/evals/run_20260415_1[4-5]*/
    // v1.26.0 per-category alphas + cross_language change from v3 sweep.
    //
    // The full v3 sweep (2026-04-16) measured best-per-category α on the
    // v3 train split, but when the new alphas were tested through the
    // PRODUCTION FULL ROUTER on v3 test, only cross_language produced a
    // real R@1 change. The others were masked by strategy routing
    // (NameOnly, DenseBase, DenseWithTypeHints) which already captures
    // most category-specific behavior.
    //
    // v3 test R@1 measurements (109 queries):
    //   v1.26.0 alphas:               44.0%
    //   full v3-swept alphas:         44.0% (0.0pp)
    //   v1.26.0 + xlang=0.10 only:    45.0% (+1.0pp)  ← shipped
    //
    // cross_language change rationale: semantic bridging across languages
    // (e.g. "Python equivalent of map in Rust") doesn't benefit from SPLADE
    // lexical matching — you need dense embeddings to cross the syntax
    // boundary. α=0.10 puts almost all weight on dense. +9pp on the
    // category's R@1 on v3 test (18.2% → 27.3%).
    //
    // Other categories' v3 sweep deltas lived mostly in Unknown queries
    // that the rule-based classifier never routes to them anyway, so the
    // optima were unreachable in production. Full data in
    // ~/training-data/research/models.md.
    //
    // Run artifacts: /mnt/c/Projects/cqs/evals/queries/v3_alpha_sweep.json
    //
    // Sourced from `QueryCategory::default_alpha`, which is generated by
    // `define_query_categories!` (see top of this file). The match in that
    // generator is exhaustive — adding a new variant without
    // `default_alpha = ...` is a compile error, so a SPLADE-tuning gap can
    // no longer slip through under a `_ => 1.0` catch-all.
    let alpha = category.default_alpha();

    // OB-V1.30.1-1 / P3-OB-1: demote per-search routing to debug — see
    // top of `resolve_splade_alpha` for the rationale.
    tracing::debug!(
        category = %category,
        alpha,
        source = "default",
        "SPLADE routing"
    );
    alpha
}

/// Pure function — no I/O, cannot fail, completes in <1ms.
/// Priority order: Negation > Identifier > CrossLanguage > TypeFiltered >
/// Structural > Behavioral > Conceptual > MultiStep > Unknown.
pub fn classify_query(query: &str) -> Classification {
    // OB-V1.29-6: entry span so trace captures show the classifier running
    // on every search query, plus a debug log at exit that records which
    // category / strategy was picked. `classify_query` is pure but sits on
    // the hot path for every search, so callers often want to confirm the
    // category assignment after the fact without recomputing it.
    let _span = tracing::info_span!("classify_query", query_len = query.len()).entered();
    let classification = classify_query_inner(query);
    tracing::debug!(
        category = %classification.category,
        confidence = ?classification.confidence,
        strategy = ?classification.strategy,
        "Query classified"
    );
    classification
}

/// Inner body of [`classify_query`] — split so the outer function can log the
/// chosen category once regardless of which branch fires. Keeps the early
/// `return` chain intact so the priority order reads top-to-bottom.
fn classify_query_inner(query: &str) -> Classification {
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
    //    2026-04-13: route to base. Enrichment ablation at 78% summary coverage
    //    showed +8.4pp R@1 on base vs enriched (41.7% vs 33.3%, N=24).
    //    Summaries add generic vocabulary that dilutes the specific type signal.
    let type_hints = extract_type_hints(&query_lower);
    if type_hints.is_some() {
        return Classification {
            category: QueryCategory::TypeFiltered,
            confidence: Confidence::Medium,
            strategy: SearchStrategy::DenseBase,
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
    //    2026-04-13: route to base. Enrichment ablation at 78% summary coverage
    //    showed +2.9pp R@1 on base vs enriched (23.5% vs 20.6%, N=34).
    //    Summaries inject vocabulary that displaces the conjunction terms.
    if MULTISTEP_PATTERNS_AC.is_match(&query_lower) {
        return Classification {
            category: QueryCategory::MultiStep,
            confidence: Confidence::Low,
            strategy: SearchStrategy::DenseBase,
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
fn is_identifier_query(query: &str, words: &[&str]) -> bool {
    // Single-word queries with identifier chars
    if words.len() == 1 {
        let w = words[0];
        // Must contain at least one letter
        if !w.chars().any(|c| c.is_alphabetic()) {
            return false;
        }
        // NL indicator words are not identifiers. On a single-word query the
        // word itself IS the whole query and carries no whitespace, so the
        // AC word-boundary match reduces to "some pattern equals w".
        if ac_has_word_bounded_match(&NL_INDICATORS_AC, query) {
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
        let has_nl = ac_has_word_bounded_match(&NL_INDICATORS_AC, query);
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
///
/// AC-V1.25-9: the translation-verb check uses word-token matching for
/// "port" / "ports" / "convert" / "translate" / "equivalent". Previously
/// the "port " substring probe false-fired on "report", "reports",
/// "airport" etc. — any word with "port " inside it at a word boundary
/// inside a longer compound would look like a translation verb.
fn is_cross_language_query(query: &str, words: &[&str]) -> bool {
    let names = language_names();
    let lang_count = names
        .iter()
        .filter(|l| words.iter().any(|w| *w == **l))
        .count();
    if lang_count >= 2 {
        return true;
    }
    let has_translate_verb = query.contains("equivalent")
        || query.contains("translate")
        || query.contains("convert ")
        || words.iter().any(|w| *w == "port" || *w == "ports");
    if lang_count >= 1 && has_translate_verb {
        return true;
    }
    false
}

/// Check if query is structural (about code structure, not behavior).
///
/// AC-V1.29-2: keyword matching uses whitespace-split word tokens so the
/// keyword fires regardless of position (`"find all trait"`, `"all class"`,
/// `"find enum"`). The previous impl required a trailing space, so any
/// keyword at end-of-query silently misrouted to Conceptual α=0.70 instead
/// of Structural α=0.90.
fn is_structural_query(query: &str) -> bool {
    // AC-V1.30.1-2 (P3-AC-1): line 643 already passes `&query_lower`, but
    // the other call sites (lines 900, 1265, 1275-1276) pass raw query
    // strings, so a query like `"Class Foo"` would miss the `class`
    // keyword match. Lowercase here once so every caller is correct
    // without per-site discipline; the AC pattern set is keyed on
    // lowercase forms anyway.
    let query_lower = query.to_ascii_lowercase();
    let query = query_lower.as_str();
    // Structural patterns like "functions that return"
    if STRUCTURAL_PATTERNS_AC.is_match(query) {
        return true;
    }
    // Contains structural keywords as NL words (not identifiers)
    // e.g., "find all structs" but not "MyStruct"
    let words: Vec<&str> = query.split_whitespace().collect();
    STRUCTURAL_KEYWORDS
        .iter()
        .any(|kw| words.iter().any(|w| w == kw))
}

/// Check if query describes behavior.
///
/// AC-V1.25-15: the "code that" / "function that" probes use word-boundary
/// checks instead of raw substring contains so hyphenated identifiers like
/// `code-that-was-deleted-yesterday` don't false-fire. The word-boundary
/// phrase must be surrounded by whitespace or sit at a string boundary.
fn is_behavioral_query(query: &str, _words: &[&str]) -> bool {
    if ac_has_word_bounded_match(&BEHAVIORAL_VERBS_AC, query) {
        return true;
    }
    // "how does" / "what does" removed 2026-04-14 — they caught 100% of
    // multi_step eval queries ("how does X trace callers to find tests")
    // and sent them down α=0.05 (Behavioral) instead of α=1.0 (MultiStep /
    // Unknown). Net loss: ~3 queries / 265. "code that" / "function that"
    // are kept — they're more specific phrasings used by genuine behavioral
    // queries ("function that embeds a batch of text documents").
    contains_phrase(query, "code that") || contains_phrase(query, "function that")
}

/// Check whether `phrase` appears in `query` surrounded by whitespace or
/// string boundaries — a word-boundary check without regex overhead.
///
/// Used by [`is_behavioral_query`] so hyphenated or compounded identifiers
/// that happen to contain the phrase as a substring don't false-fire.
fn contains_phrase(query: &str, phrase: &str) -> bool {
    let bytes = query.as_bytes();
    let pbytes = phrase.as_bytes();
    let plen = pbytes.len();
    if plen == 0 || bytes.len() < plen {
        return false;
    }
    for start in 0..=bytes.len() - plen {
        if &bytes[start..start + plen] != pbytes {
            continue;
        }
        let left_ok = start == 0 || bytes[start - 1].is_ascii_whitespace();
        let right_ok = start + plen == bytes.len() || bytes[start + plen].is_ascii_whitespace();
        if left_ok && right_ok {
            return true;
        }
    }
    false
}

/// Check whether any pattern in `ac` has at least one whole-word match in
/// `query`. A match is whole-word iff both sides of the match are either
/// a string boundary or ASCII whitespace.
///
/// Used in place of the previous `words.iter().any(|w| SET.contains(w))`
/// check: tokens split by whitespace are exactly the strings whose first
/// and last bytes sit at ASCII whitespace (or the string boundary), so an
/// AC match with whitespace on both sides represents a token that equals
/// one of the patterns. No regex, no allocation, single pass over `query`.
///
/// Uses [`AhoCorasick::find_overlapping_iter`] so shared-prefix patterns
/// (e.g. `"a"` / `"all"` / `"an"` in [`NL_INDICATORS`]) all get a chance
/// to fire: a leftmost-first `find_iter` would return the first pattern
/// that matches at position 0, and if that pattern is not word-bounded
/// the helper would wrongly report "no match" even when a longer sibling
/// pattern *is* word-bounded at the same start. Requires
/// [`MatchKind::Standard`], which is the default for [`AhoCorasick::new`].
fn ac_has_word_bounded_match(ac: &AhoCorasick, query: &str) -> bool {
    let bytes = query.as_bytes();
    for m in ac.find_overlapping_iter(query) {
        let left_ok = m.start() == 0 || bytes[m.start() - 1].is_ascii_whitespace();
        let right_ok = m.end() == bytes.len() || bytes[m.end()].is_ascii_whitespace();
        if left_ok && right_ok {
            return true;
        }
    }
    false
}

/// Check if query is about abstract concepts.
fn is_conceptual_query(query: &str, words: &[&str]) -> bool {
    if ac_has_word_bounded_match(&CONCEPTUAL_NOUNS_AC, query) {
        return true;
    }
    // Short queries (1-3 words) that aren't identifiers and aren't structural
    words.len() <= 3
        && ac_has_word_bounded_match(&NL_INDICATORS_AC, query)
        && !is_structural_query(query)
}

/// Pre-computed `(phrase, chunk_type)` table assembled from
/// `ChunkType::ALL.iter().flat_map(|ct| ct.hint_phrases().iter().map(|p| (*p, *ct)))`.
///
/// Order is determined by `ChunkType::ALL` (declaration order in
/// `define_chunk_types!`) and within a variant by the order of phrases in its
/// `hints = [...]` list. `extract_type_hints` walks the table in this order to
/// preserve hint sequencing in the output `Vec`.
///
/// Materialized once at first use. Adding a new `ChunkType` variant with
/// `hints = [...]` automatically appears here — there is no second registration
/// step in `router.rs`.
static TYPE_HINT_TABLE: LazyLock<Vec<(&'static str, ChunkType)>> = LazyLock::new(|| {
    ChunkType::ALL
        .iter()
        .flat_map(|ct| ct.hint_phrases().iter().map(move |p| (*p, *ct)))
        .collect()
});

/// Aho-Corasick automaton over [`TYPE_HINT_TABLE`] — one pass over
/// `query` finds every matching pattern id.
///
/// Uses [`MatchKind::Standard`] because [`AhoCorasick::find_overlapping_iter`]
/// (which we need: sibling patterns like `"constructor"` / `"all constructors"`
/// overlap in the haystack, and both must fire to match the previous
/// `for (pat, _) in patterns { if query.contains(pat) {..} }` semantics)
/// is only valid under the Standard match kind.
static TYPE_HINT_AC: LazyLock<AhoCorasick> = LazyLock::new(|| {
    AhoCorasickBuilder::new()
        .match_kind(MatchKind::Standard)
        .build(TYPE_HINT_TABLE.iter().map(|(p, _)| *p))
        .expect("TYPE_HINT_TABLE is a valid pattern set (static input)")
});

/// Extract chunk type hints from the query text.
///
/// Returns the types to boost (not filter) in search results.
/// Only extracts when confidence is reasonable — avoids false positives.
///
/// Previously this scanned ~72 patterns with individual `query.contains(p)`
/// probes. Now uses a single Aho-Corasick pass via [`TYPE_HINT_AC`], with the
/// `(phrase, ChunkType)` table built from `ChunkType::hint_phrases()` declared
/// in `define_chunk_types!` — a single source of truth for hint registration.
///
/// Output order is preserved: a hint is pushed the first time its pattern
/// id appears in declaration order, and duplicate `ChunkType`s across
/// different matched patterns are kept (e.g. two Test-mapped patterns both
/// matching still yields `[Test, Test]`, matching the previous loop).
pub fn extract_type_hints(query: &str) -> Option<Vec<ChunkType>> {
    let table = &*TYPE_HINT_TABLE;
    // Collect the set of pattern ids that match at least once.
    let mut matched = vec![false; table.len()];
    for m in TYPE_HINT_AC.find_overlapping_iter(query) {
        matched[m.pattern().as_usize()] = true;
    }

    let mut types = Vec::new();
    for (idx, (_, chunk_type)) in table.iter().enumerate() {
        if matched[idx] {
            types.push(*chunk_type);
        }
    }

    if types.is_empty() {
        None
    } else {
        Some(types)
    }
}

// ── Centroid classifier ─────────────────────────────────────────────
//
// Embedding-space centroid matching: 8 pre-computed category centroids
// from the v3 eval train split (326 queries, BGE-large 1024-dim).
// At query time, cosine-sim to each centroid; if top-1 margin over
// top-2 exceeds θ, override the rule-based category. θ=0.01 gives
// 87.7% accuracy at 67% coverage on the dev split; below θ the
// rule-based classifier handles it.
//
// Centroid file: ~/.local/share/cqs/classifier_centroids.v1.json
// Override: CQS_CENTROID_THRESHOLD (default 0.01)
// Disable:  CQS_CENTROID_CLASSIFIER=0

static CENTROID_CLASSIFIER: OnceLock<Option<CentroidClassifier>> = OnceLock::new();

struct CentroidClassifier {
    centroids: HashMap<QueryCategory, Vec<f32>>,
    dim: usize,
    threshold: f32,
}

impl CentroidClassifier {
    fn load() -> Option<Self> {
        if std::env::var("CQS_CENTROID_CLASSIFIER")
            .map(|v| v == "0")
            .unwrap_or(false)
        {
            tracing::info!("centroid classifier disabled via CQS_CENTROID_CLASSIFIER=0");
            return None;
        }

        let path = dirs::data_dir()?.join("cqs/classifier_centroids.v1.json");
        // RM-V1.33-9: cap centroid file reads at 16 MiB. Real centroid
        // registries are 50-200 KiB even with 100 categories × 1024-dim
        // floats; 16 MiB rejects hostile or corrupted multi-GB files
        // before the daemon OOMs at first search.
        use std::io::Read;
        const MAX_CENTROID_BYTES: u64 = 16 * 1024 * 1024;
        let mut text = String::new();
        let read_result = std::fs::File::open(&path)
            .and_then(|f| f.take(MAX_CENTROID_BYTES).read_to_string(&mut text));
        if let Err(e) = read_result {
            tracing::debug!(path = %path.display(), error = %e, "centroid file not found — rule-only mode");
            return None;
        }
        let data: serde_json::Value = match serde_json::from_str(&text) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(error = %e, "centroid file parse failed");
                return None;
            }
        };

        let dim = data["dim"].as_u64()? as usize;
        let categories = data["categories"].as_object()?;
        let mut centroids = HashMap::new();
        for (name, obj) in categories {
            let cat = QueryCategory::from_snake_case(name)?;
            let arr = obj["centroid"].as_array()?;
            let vec: Vec<f32> = arr
                .iter()
                .filter_map(|v| v.as_f64().map(|f| f as f32))
                .collect();
            if vec.len() != dim {
                tracing::warn!(
                    category = name,
                    expected = dim,
                    got = vec.len(),
                    "centroid dim mismatch"
                );
                continue;
            }
            centroids.insert(cat, vec);
        }

        if centroids.is_empty() {
            // P3 #94: include the path and expected dim so an operator can
            // tell whether the file is at the wrong location, was generated
            // for a different embedding model, or is genuinely empty.
            tracing::warn!(
                path = %path.display(),
                expected_dim = dim,
                "centroid file contained 0 valid centroids"
            );
            return None;
        }

        let threshold = std::env::var("CQS_CENTROID_THRESHOLD")
            .ok()
            .and_then(|v| v.parse::<f32>().ok())
            .unwrap_or(0.01);

        tracing::info!(
            n_centroids = centroids.len(),
            dim,
            threshold,
            "centroid classifier loaded"
        );
        Some(CentroidClassifier {
            centroids,
            dim,
            threshold,
        })
    }

    fn classify(&self, embedding: &[f32]) -> Option<(QueryCategory, f32)> {
        if embedding.len() != self.dim {
            return None;
        }
        let mut best_cat = QueryCategory::Unknown;
        let mut best_score = f32::NEG_INFINITY;
        let mut second_score = f32::NEG_INFINITY;

        for (&cat, centroid) in &self.centroids {
            let score: f32 = embedding
                .iter()
                .zip(centroid.iter())
                .map(|(a, b)| a * b)
                .sum();
            if score > best_score {
                second_score = best_score;
                best_score = score;
                best_cat = cat;
            } else if score > second_score {
                second_score = score;
            }
        }

        let margin = best_score - second_score;
        if margin >= self.threshold {
            Some((best_cat, margin))
        } else {
            None
        }
    }
}

/// Upgrade a rule-based classification using embedding-space centroids.
///
/// **Enabled by default as of v1.28.2** — A/B on v3.v2 (109 queries each
/// split, 2026-04-20) showed test R@5 +3.7pp, dev R@5 ±0, with category
/// breakdown:
///
///   - structural_search:  +12.5pp (n=16)
///   - cross_language:     +9.1pp  (n=22)
///   - behavioral_search:  +3.1pp  (n=32)
///   - identifier_lookup, multi_step, negation, type_filtered: ±0
///   - conceptual_search:  −4.0pp  (n=25, single-query noise on low base)
///
/// The earlier v3-2026-04-15 −4.6pp R@1 regression was eliminated by the
/// alpha floor (CENTROID_ALPHA_FLOOR=0.7) added before this measurement.
/// Set `CQS_CENTROID_CLASSIFIER=0` to opt out.
///
/// Call AFTER the query embedding is available.
pub fn reclassify_with_centroid(
    mut classification: Classification,
    embedding: &[f32],
) -> Classification {
    // OB-V1.29-6: entry span so the centroid-upgrade step is visible in
    // traces separately from the outer rule-based classify.
    let _span = tracing::info_span!("reclassify_with_centroid").entered();
    // Centroid classifier: fills Unknown gaps with embedding-space classification.
    // Alpha-clipped: centroid-assigned α is clamped to ≥ CENTROID_ALPHA_FLOOR
    // so misclassifications can't catastrophically zero out SPLADE (the −4.6pp
    // regression from v3 eval was entirely from Behavioral α=0.0 assignments).
    //
    // Env: CQS_CENTROID_CLASSIFIER=0 to disable entirely.
    //      CQS_CENTROID_ALPHA_FLOOR (default 0.7) — minimum α for centroid-assigned categories.
    if std::env::var("CQS_CENTROID_CLASSIFIER")
        .map(|v| v == "0")
        .unwrap_or(false)
    {
        return classification;
    }
    if classification.category != QueryCategory::Unknown {
        return classification;
    }
    let classifier = CENTROID_CLASSIFIER.get_or_init(CentroidClassifier::load);
    if let Some(cls) = classifier {
        if let Some((cat, margin)) = cls.classify(embedding) {
            // OB-V1.30.1-2 / P3-OB-1: per-search event — demote to debug
            // so the operator default log level isn't flooded; the
            // surrounding `info_span!`s still carry the trace context.
            tracing::debug!(
                centroid_category = %cat,
                margin = format!("{margin:.4}"),
                "centroid filled Unknown gap"
            );
            classification.category = cat;
            classification.confidence = Confidence::Medium;
        }
    }
    classification
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

    /// P2.44 regression-pin: structural keywords at end-of-query must
    /// classify as Structural. The previous impl wrapped the keyword in
    /// `format!(" {} ", kw)` and looked for that as a substring — any
    /// keyword sitting at the end of the string failed the substring
    /// search and silently misrouted to Conceptual α=0.70.
    #[test]
    fn test_p2_44_structural_keywords_at_end_of_query() {
        for q in &[
            "find all trait",
            "show me all trait",
            "find every impl",
            "list all enum",
            "all class",
            "find enum",
        ] {
            assert!(
                is_structural_query(q),
                "query `{q}` must classify as structural"
            );
        }
    }

    /// P2.44 regression-pin: structural keywords as substrings of normal
    /// words ("training" contains "trait") must NOT false-fire structural.
    #[test]
    fn test_p2_44_structural_keyword_as_substring_does_not_match() {
        assert!(!is_structural_query("training pipeline"));
        assert!(!is_structural_query("classifier"));
    }

    /// AC-V1.30.1-2 (P3-AC-1) regression-pin: case-folding inside
    /// `is_structural_query` so callers that pass raw queries (line 900
    /// in `is_conceptual_query`, the centroid path, and external test
    /// callers) classify uppercase or mixed-case structural keywords
    /// correctly. The pre-fix path only worked when callers had already
    /// lowercased — `"Class Foo"` and `"FIND ALL STRUCT"` would miss.
    /// Note: `STRUCTURAL_KEYWORDS` is singular-only (`struct`, not
    /// `structs`); the case-fold pin tracks the uppercase axis only.
    #[test]
    fn test_ac_v1_30_1_2_is_structural_query_case_folds() {
        assert!(is_structural_query("Class Foo"));
        assert!(is_structural_query("Trait Iterator"));
        assert!(is_structural_query("FIND ALL STRUCT"));
        assert!(is_structural_query("Find Every Enum"));
        // negative pin: substring + uppercase still must not false-fire
        assert!(!is_structural_query("Training Pipeline"));
    }

    #[test]
    fn test_classify_type_filtered() {
        let c = classify_query("all test functions");
        assert_eq!(c.category, QueryCategory::TypeFiltered);
        // 2026-04-13: type_filtered routes to base — summaries dilute type signal (+8.4pp).
        assert_eq!(c.strategy, SearchStrategy::DenseBase);
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
        // 2026-04-13: multi_step routes to base — summaries displace conjunction terms (+2.9pp).
        assert_eq!(c.strategy, SearchStrategy::DenseBase);
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

    // ── AC-V1.25-10 MultiStep pattern tightening ─────────────────────

    #[test]
    fn test_classify_plain_and_is_not_multistep() {
        // "find foo and bar" is a single search intent, not a multi-step
        // query. Previously " and " alone pushed this into MultiStep.
        let c = classify_query("find foo and bar");
        assert_ne!(
            c.category,
            QueryCategory::MultiStep,
            "plain conjunction should not classify as MultiStep"
        );
    }

    #[test]
    fn test_classify_plain_or_is_not_multistep() {
        // "find foo or bar" is a single search intent with alternation,
        // not a multi-step query.
        let c = classify_query("find foo or bar");
        assert_ne!(
            c.category,
            QueryCategory::MultiStep,
            "plain disjunction should not classify as MultiStep"
        );
    }

    #[test]
    fn test_classify_first_then_is_multistep() {
        // Explicit sequencing must still classify as MultiStep.
        let c = classify_query("first do X then do Y");
        assert_eq!(c.category, QueryCategory::MultiStep);
    }

    #[test]
    fn test_classify_and_then_is_multistep() {
        // "and then" explicitly chains two steps.
        let c = classify_query("find errors and then retry them");
        assert_eq!(c.category, QueryCategory::MultiStep);
    }

    // ── AC-V1.25-9 cross-language classifier word-boundary ──────────

    #[test]
    fn test_classify_report_is_not_cross_language() {
        // Previously "port " substring probe matched "report" and
        // classified any language + "report" query as CrossLanguage.
        let c = classify_query("show the error report in python");
        assert_ne!(
            c.category,
            QueryCategory::CrossLanguage,
            "'report' should not trigger cross-language via 'port ' substring"
        );
    }

    #[test]
    fn test_classify_port_verb_stays_cross_language() {
        // "port X to Y" with a language name is still CrossLanguage.
        let c = classify_query("port the logging module to rust");
        assert_eq!(c.category, QueryCategory::CrossLanguage);
    }

    // ── AC-V1.25-15 behavioral classifier word-boundary ─────────────

    #[test]
    fn test_classify_word_bounded_code_that_not_behavioral() {
        // A token-attached "code that" like `barcode that1` contains the
        // literal "code that" substring but is not the phrase "code that"
        // as a word. Previously the substring probe classified this as
        // Behavioral; the word-boundary check should not.
        let c = classify_query("barcode that1 lives forever");
        assert_ne!(
            c.category,
            QueryCategory::Behavioral,
            "token-attached 'code that' should not classify as Behavioral via substring"
        );
    }

    #[test]
    fn test_classify_word_bounded_function_that_not_behavioral() {
        // "malfunction that" attaches "function that" to "mal"; should
        // not match the word-bounded phrase check.
        let c = classify_query("malfunction that3 happened");
        assert_ne!(
            c.category,
            QueryCategory::Behavioral,
            "token-attached 'function that' should not classify as Behavioral"
        );
    }

    #[test]
    fn test_classify_behavioral_code_that_still_fires() {
        // "code that ..." as a real NL phrase still classifies as
        // Behavioral after word-boundary tightening.
        let c = classify_query("code that handles retries");
        assert_eq!(c.category, QueryCategory::Behavioral);
    }

    // ── AC-V1.29-2 structural keyword end-of-query ──────────────────

    #[test]
    fn test_structural_keyword_at_end_of_query() {
        // The previous probe required a trailing space, so keywords at
        // end-of-query silently misrouted to Conceptual. After the
        // whitespace-split rewrite, every position matches.
        assert!(
            is_structural_query("find all trait"),
            "trailing 'trait' should classify as structural"
        );
        assert!(
            is_structural_query("all class"),
            "trailing 'class' should classify as structural"
        );
        assert!(
            is_structural_query("find enum"),
            "trailing 'enum' should classify as structural"
        );
    }

    #[test]
    fn test_structural_keyword_single_word_query() {
        // A single-keyword query (just "trait") is structural — previously
        // required surrounding spaces that a one-word query doesn't have.
        assert!(is_structural_query("trait"));
        assert!(is_structural_query("struct"));
    }

    #[test]
    fn test_structural_keyword_middle_still_works() {
        // Keyword in the middle of a query was already matched by the
        // " {kw} " probe. Regression-cover it so the rewrite doesn't
        // silently drop this case.
        assert!(is_structural_query("find all trait implementations"));
        assert!(is_structural_query("every class definition"));
    }

    #[test]
    fn test_structural_keyword_substring_does_not_fire() {
        // Word-split matching: "MyTraitImpl" as a CamelCase identifier does
        // NOT classify as structural just because it contains "trait".
        assert!(!is_structural_query("MyTraitImpl"));
    }

    // ── Micro-benchmark (#964) ───────────────────────────────────────
    //
    // Sanity check for the Aho-Corasick + LazyLock rewrite. Runs
    // classify_query on a mix of query shapes and prints per-call
    // timing. Does not assert on timing — CI machines have wildly
    // different performance envelopes.
    //
    // Marked #[ignore] so the default `cargo test` run does not pay
    // the timing cost; invoke in release for a realistic number:
    //   cargo test --release --features gpu-index --lib -- \
    //     search::router::tests::bench_classify_query_throughput \
    //     --ignored --nocapture
    #[test]
    #[ignore]
    fn bench_classify_query_throughput() {
        // Four query shapes that exercise different branches of classify_query:
        //   1. Type-filtered — runs the full 72-pattern extract_type_hints
        //      table, which was the heaviest contributor before the AC rewrite.
        //   2. Behavioral — fires on a BEHAVIORAL_VERBS word.
        //   3. Cross-language — two language names, full language_names scan.
        //   4. Unknown — walks every branch (no early return) so the whole
        //      classifier is stressed.
        let queries: &[(&str, &str)] = &[
            (
                "type_filtered",
                "find all test functions and every interface and all traits in the codebase shown",
            ),
            (
                "behavioral",
                "find the function that validates user input in the python module and logs it",
            ),
            (
                "cross_language",
                "port the python logging and tracing module into a rust crate with serde",
            ),
            (
                "unknown",
                "zephyr quartz wonder blooming river sunset gentle breeze stormy afternoon light",
            ),
        ];

        // Warm the LazyLocks so construction cost isn't folded into timing.
        for (_, q) in queries {
            let _ = classify_query(q);
        }

        const ITERATIONS: usize = 10_000;
        for (label, query) in queries {
            assert!(
                query.len() >= 60 && query.len() <= 95,
                "keep bench queries near the 80-char target ({} = {} chars)",
                label,
                query.len()
            );
            let start = std::time::Instant::now();
            let mut sink = 0u32;
            for _ in 0..ITERATIONS {
                let c = classify_query(query);
                // Prevent the optimizer from eliding the call.
                sink = sink.wrapping_add(c.category as u32);
            }
            let elapsed = start.elapsed();
            let per_call_ns = elapsed.as_nanos() / ITERATIONS as u128;
            eprintln!(
                "classify_query bench [{:<14}]: {} iters in {:>9.3?} ({:>5} ns/call, sink={})",
                label, ITERATIONS, elapsed, per_call_ns, sink
            );
        }
    }
}
