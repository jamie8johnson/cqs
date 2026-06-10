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
// Adding a category = one new line here. The `default_alpha` match is
// exhaustive: a missing `default_alpha = ...` is a compile error, so a
// SPLADE-tuning gap can't ship invisibly under a `_ => 1.0` catch-all.
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
        /// the on-disk eval JSON carries both `"behavioral"` and
        /// `"behavioral_search"` for the same variant.
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
            /// Sourced from per-category sweeps. Exhaustive — adding a new
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
    /// Looking for a specific function/type by name ("search_filtered", "HashMap::new").
    /// Routes to `SearchStrategy::NameOnly` which bypasses SPLADE entirely, so
    /// alpha only applies on the rare fall-through. 0.85 sits in the middle of
    /// the flat α=0.80..0.90 plateau and tolerates classifier drift without
    /// falling off either edge.
    IdentifierLookup => "identifier_lookup", default_alpha = 0.85;
    /// Searching for code by structure ("functions that return Result", "structs with Display").
    /// 0.6 leans SPLADE because structural signals ("returns Result", "Display
    /// impl") surface in the sparse encoder's lexical features; pure dense
    /// collapses recall on these queries.
    Structural => "structural", default_alpha = 0.60, aliases = ["structural_search"];
    /// Searching for code by behavior ("validates user input", "retries with backoff").
    /// Pure dense: behavioral query embeddings are strong enough that adding any
    /// SPLADE weight hurts. Production lift is bottlenecked by `behavioral`
    /// classifier accuracy (~19% fire rate).
    Behavioral => "behavioral", default_alpha = 1.00, aliases = ["behavioral_search"];
    /// Searching for abstract concepts ("dependency injection", "observer pattern").
    /// Mostly dense — abstract concept queries embed well; the small SPLADE
    /// weight catches the few queries with token overlap to specific impls.
    Conceptual => "conceptual", default_alpha = 0.80, aliases = ["conceptual_search"];
    /// Queries requiring multiple signals ("find where errors are logged and retried").
    /// Heavily SPLADE: multi-clause queries have heavy keyword overlap that
    /// SPLADE catches well at depth. The rule-based classifier rarely fires
    /// `multi_step` correctly because "X AND Y" patterns trip the structural
    /// rule first.
    MultiStep => "multi_step", default_alpha = 0.10;
    /// Queries with negation ("sort without allocating", "parse but not validate").
    /// 0.8 sits inside the flat region of the α curve with a comfortable margin
    /// from the dense-only edge, which fails on these queries.
    Negation => "negation", default_alpha = 0.80;
    /// Queries constrained by chunk type ("all test functions", "every enum").
    /// Pure SPLADE: type-filter queries are dominated by lexical signals
    /// ("test function", "enum variant"); dense embeddings add noise that
    /// SPLADE's term weights filter out cleanly.
    TypeFiltered => "type_filtered", default_alpha = 0.00;
    /// Queries mentioning multiple languages ("Python equivalent of map in Rust").
    /// Dense-leaning fusion: bilingual embeddings bridge the syntax boundary,
    /// while the SPLADE weight keeps exact language-name matching as a tiebreaker.
    CrossLanguage => "cross_language", default_alpha = 0.70;
    /// No clear category — the catch-all bucket where the rule-based classifier
    /// deposits queries it doesn't recognise, including many MISCLASSIFIED
    /// queries (structural-style queries the rule chain doesn't fire on) whose
    /// true category's α never reaches them.
    ///
    /// 0.80 is the joint optimum on mean R@5 and hedges both cases: misroutes
    /// still get most of the SPLADE+dense fusion benefit, and genuine
    /// "no signal" queries also benefit over pure dense.
    Unknown => "unknown", default_alpha = 0.80;
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
    /// Standard dense embedding search (default path, enriched HNSW)
    DenseDefault,
    /// Dense search with type boost for matching chunk types (enriched HNSW)
    DenseWithTypeHints,
    /// Dense search against the base (non-enriched) HNSW — LLM summaries hurt
    /// conceptual/behavioral/negation signal because they inject canonical
    /// vocabulary that drowns out query semantics. Falls back to
    /// [`Self::DenseDefault`] when the base index is missing.
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

/// Negation tokens matched against word-split query tokens (not substrings),
/// so words like `cannot`, `piano`, `nano` don't false-fire.
///
/// The compile-time floor below is the default; operators add domain phrases
/// (`ignoring`, `without using`, etc.) via [`install_classifier_vocab_overlay`]
/// from `~/.config/cqs/classifier.toml` and `<project>/.cqs/classifier.toml`.
fn builtin_negation_tokens() -> std::collections::HashSet<String> {
    [
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
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Runtime-extensible negation token set. Initialized lazily with the
/// compile-time floor; merged with TOML-overlay extras via
/// [`install_classifier_vocab_overlay`]. Token lookups are word-exact
/// case-sensitive (the upstream `words` vec is already lowercased before
/// the lookup, so set entries are stored lowercase too).
static NEGATION_TOKENS: LazyLock<std::sync::RwLock<std::collections::HashSet<String>>> =
    LazyLock::new(|| std::sync::RwLock::new(builtin_negation_tokens()));

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
/// returns a borrow of the same `Vec`.
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

/// Aho-Corasick automaton over [`STRUCTURAL_PATTERNS`]. Matched as raw
/// substrings, so any match — word-bounded or not — triggers structural
/// classification.
static STRUCTURAL_PATTERNS_AC: LazyLock<AhoCorasick> = LazyLock::new(|| {
    AhoCorasick::new(STRUCTURAL_PATTERNS)
        .expect("STRUCTURAL_PATTERNS is a valid pattern set (static)")
});

/// Multi-step conjunction patterns.
///
/// Bare " and " / " or " are excluded — they fire on any conjunction
/// ("find foo and bar") and would sweep near-every multi-word NL query into
/// `QueryCategory::MultiStep`. These patterns require explicit sequencing /
/// enumeration phrasing ("first do X then do Y") so the category captures
/// multi-step intent, not any coordinated phrase.
///
/// Operators extend via TOML overlay (e.g. ordering verbs in non-English
/// domain queries) without a rebuild.
fn builtin_multistep_patterns() -> Vec<String> {
    [
        "and then",
        "before ",
        "after ",
        " or also ",
        "first ",
        "then ",
        "both ",
        "between ",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Aho-Corasick automaton over multistep patterns. Built once on first
/// access, rebuilt by [`install_classifier_vocab_overlay`] when an overlay
/// merges in extra patterns. `RwLock<Arc<AhoCorasick>>` so the hot search
/// path (`is_match`) just clones the inner Arc — cheap, stable across the
/// rest of the call.
static MULTISTEP_PATTERNS_AC: LazyLock<std::sync::RwLock<std::sync::Arc<AhoCorasick>>> =
    LazyLock::new(|| {
        let patterns = builtin_multistep_patterns();
        let ac =
            AhoCorasick::new(&patterns).expect("builtin MULTISTEP_PATTERNS is a valid pattern set");
        std::sync::RwLock::new(std::sync::Arc::new(ac))
    });

/// Merge a classifier-vocab overlay into the runtime NEGATION + MULTISTEP
/// sets. Called once at CLI/daemon startup after parsing
/// `~/.config/cqs/classifier.toml` (user-global) and
/// `<project>/.cqs/classifier.toml` (project-local; layered on top).
///
/// `extra_negation` entries are lowercased and merged into the negation
/// set. `extra_multistep` entries are merged with the builtin patterns
/// and a new `AhoCorasick` automaton is built — empty/whitespace-only
/// entries are dropped at the loader.
///
/// Both vecs empty → no-op.
pub fn install_classifier_vocab_overlay(extra_negation: Vec<String>, extra_multistep: Vec<String>) {
    if extra_negation.is_empty() && extra_multistep.is_empty() {
        return;
    }
    // Info-level so operators editing `~/.config/cqs/classifier.toml` see
    // their config land in journald without RUST_LOG=debug. Fires only when
    // at least one set is non-empty.
    let neg_count = extra_negation.len();
    let multi_count = extra_multistep.len();
    if !extra_negation.is_empty() {
        let mut g = NEGATION_TOKENS.write().unwrap_or_else(|p| p.into_inner());
        for tok in extra_negation {
            g.insert(tok.to_lowercase());
        }
    }
    if !extra_multistep.is_empty() {
        // Rebuild the AC over the union of builtins + overlay so the
        // overlay extends rather than replaces the set.
        let mut all = builtin_multistep_patterns();
        for pat in extra_multistep {
            if !pat.trim().is_empty() && !all.contains(&pat) {
                all.push(pat);
            }
        }
        let new_ac = match AhoCorasick::new(&all) {
            Ok(ac) => ac,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "Classifier vocab overlay multistep patterns rejected by AhoCorasick — \
                     keeping builtin AC"
                );
                return;
            }
        };
        let mut g = MULTISTEP_PATTERNS_AC
            .write()
            .unwrap_or_else(|p| p.into_inner());
        *g = std::sync::Arc::new(new_ac);
    }
    tracing::info!(
        negation_added = neg_count,
        multistep_added = multi_count,
        "Installed classifier vocab overlay"
    );
}

/// Test-only: reset both vocab tables to the compile-time builtins.
#[cfg(test)]
pub(crate) fn reset_classifier_vocab_for_test() {
    {
        let mut g = NEGATION_TOKENS.write().unwrap_or_else(|p| p.into_inner());
        *g = builtin_negation_tokens();
    }
    {
        let patterns = builtin_multistep_patterns();
        let ac = AhoCorasick::new(&patterns).expect("rebuild builtin AC");
        let mut g = MULTISTEP_PATTERNS_AC
            .write()
            .unwrap_or_else(|p| p.into_inner());
        *g = std::sync::Arc::new(ac);
    }
}

/// Parse a `classifier.toml` overlay from disk.
///
/// Schema:
/// ```toml
/// [classifier]
/// negation_tokens = ["ignoring", "without using"]
/// multistep_patterns = ["sequentially", "step by step"]
/// ```
///
/// Returns `(extra_negation, extra_multistep)`. Missing file → `(empty,
/// empty)` (no warn). Malformed TOML → warn + empty. Bounded read at 4 KiB.
pub fn load_classifier_vocab_overlay(path: &std::path::Path) -> (Vec<String>, Vec<String>) {
    use std::io::Read;
    const MAX_BYTES: u64 = 4096;

    let mut file = match std::fs::File::open(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Vec::new(), Vec::new()),
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "Failed to open classifier vocab overlay; falling back to builtins"
            );
            return (Vec::new(), Vec::new());
        }
        Ok(f) => f,
    };
    let mut raw = String::new();
    if let Err(e) = (&mut file).take(MAX_BYTES).read_to_string(&mut raw) {
        tracing::warn!(
            path = %path.display(),
            error = %e,
            "Bounded read of classifier vocab failed; falling back to builtins"
        );
        return (Vec::new(), Vec::new());
    }

    #[derive(serde::Deserialize)]
    struct File {
        classifier: Option<ClassifierSection>,
    }
    #[derive(serde::Deserialize)]
    struct ClassifierSection {
        #[serde(default)]
        negation_tokens: Vec<String>,
        #[serde(default)]
        multistep_patterns: Vec<String>,
    }

    let parsed: File = match toml::from_str(&raw) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "Classifier vocab overlay is malformed TOML; falling back to builtins"
            );
            return (Vec::new(), Vec::new());
        }
    };

    let section = match parsed.classifier {
        Some(s) => s,
        None => return (Vec::new(), Vec::new()),
    };

    // Drop empty / whitespace-only entries before they reach the
    // AhoCorasick builder — the builder can panic on `""` and any
    // whitespace-only pattern would either match constantly or be useless.
    let neg: Vec<String> = section
        .negation_tokens
        .into_iter()
        .filter(|s| !s.trim().is_empty())
        .collect();
    let multi: Vec<String> = section
        .multistep_patterns
        .into_iter()
        .filter(|s| !s.trim().is_empty())
        .collect();
    if !neg.is_empty() || !multi.is_empty() {
        tracing::debug!(
            path = %path.display(),
            negation_added = neg.len(),
            multistep_added = multi.len(),
            "Loaded classifier vocab overlay"
        );
    }
    (neg, multi)
}

// ── Classification ───────────────────────────────────────────────────

/// Per-slot SPLADE α overrides. Loaded once at CLI/daemon startup from
/// `.cqs/slots/<active>/slot.toml` `[splade.alpha]` and consulted by
/// [`resolve_splade_alpha`] between the env precedence and the hardcoded
/// per-category default.
///
/// `RwLock<Option<HashMap>>` so writes are single-shot and reads are
/// uncontended on the hot search path. The Option distinguishes "never
/// initialised" (None — no slot context resolved, e.g. early test setup)
/// from "initialised but empty" (Some(empty) — slot has no `[splade.alpha]`
/// section, fall through is intended).
static SLOT_SPLADE_ALPHA: std::sync::RwLock<Option<std::collections::HashMap<String, f32>>> =
    std::sync::RwLock::new(None);

/// Install the per-slot SPLADE α override table for this process.
///
/// Called once by `dispatch::run_with` after the active slot is resolved;
/// the daemon also calls this at startup. Re-calls overwrite (e.g. a
/// test resetting state). Production callers expect single-shot.
///
/// `table` keys are lowercase category names (matching `QueryCategory`'s
/// `to_string()`); values are pre-validated to `[0.0, 1.0]` finite by
/// [`crate::slot::read_slot_splade_alpha_table`].
pub fn install_slot_splade_alpha_overrides(table: std::collections::HashMap<String, f32>) {
    // Info-level when slot α overrides actually take, so the journald audit
    // trail records whether the `slot.toml [splade.alpha]` table was applied.
    // Empty tables stay silent — every dispatch installs an empty table when
    // no overlay exists, and we don't want a per-command log line for that.
    let entries = table.len();
    match SLOT_SPLADE_ALPHA.write() {
        Ok(mut g) => {
            *g = Some(table);
            if entries > 0 {
                tracing::info!(entries, "Installed per-slot SPLADE α overrides");
            }
        }
        Err(_) => {
            tracing::warn!(
                "router slot α RwLock poisoned — leaving slot overrides at previous state"
            );
        }
    }
}

/// Clear the per-slot SPLADE α overrides. Test-only convenience so unit
/// tests don't leak across each other; production never needs this
/// (process-lifetime state is fine).
#[cfg(test)]
pub(crate) fn clear_slot_splade_alpha_overrides() {
    if let Ok(mut g) = SLOT_SPLADE_ALPHA.write() {
        *g = None;
    }
}

/// Resolve the SPLADE fusion alpha for a query category.
///
/// Precedence:
/// 1. Per-category env (`CQS_SPLADE_ALPHA_{CATEGORY}`)
/// 2. Global env (`CQS_SPLADE_ALPHA`)
/// 3. Per-slot `slot.toml [splade.alpha].<category>` (installed via
///    [`install_slot_splade_alpha_overrides`])
/// 4. Hardcoded per-category default (`category.default_alpha()`)
///
/// Returns a value in [0.0, 1.0] where 1.0 means pure dense and < 1.0 activates
/// SPLADE with that fusion weight.
///
/// Emits a single structured `tracing::debug!` recording the resolved alpha,
/// its source (`per_cat_env` / `global_env` / `slot_toml` / `default`), and
/// the category, so callers don't log the decision themselves and the
/// precedence stays visible.
pub fn resolve_splade_alpha(category: &QueryCategory) -> f32 {
    let _span = tracing::debug_span!("resolve_splade_alpha", category = %category).entered();

    // Per-category env override: CQS_SPLADE_ALPHA_CONCEPTUAL_SEARCH etc.
    //
    // The happy-path arm keeps the inner `if alpha.is_finite()` so the
    // non-finite warning can re-use the already-parsed `alpha` without a
    // second `parse::<f32>()` round.
    let cat_key = format!("CQS_SPLADE_ALPHA_{}", category.to_string().to_uppercase());
    #[allow(clippy::collapsible_match)]
    match std::env::var(&cat_key) {
        Ok(val) if let Ok(alpha) = val.parse::<f32>() => {
            if alpha.is_finite() {
                let alpha = alpha.clamp(0.0, 1.0);
                // Per-search routing fires on every query — the entry
                // `info_span!` carries traceability; the inner event is debug
                // so the operator default log level isn't flooded.
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
        // Surface NotUnicode separately. NotPresent is the silent default;
        // NotUnicode is operator misconfiguration.
        Err(std::env::VarError::NotPresent) => {}
        Err(e) => {
            tracing::warn!(
                var = %cat_key,
                error = %e,
                "Per-cat SPLADE alpha env var has non-unicode value — ignored"
            );
        }
    }

    // Global env override: CQS_SPLADE_ALPHA
    //
    // Mirrors the per-cat arm's structure so a malformed `CQS_SPLADE_ALPHA=NaN`
    // / `CQS_SPLADE_ALPHA=foo` / non-unicode value warns instead of silently
    // falling through (e.g. a typoed `CQS_SPLADE_ALPHA=O.7` with capital O).
    #[allow(clippy::collapsible_match)]
    match std::env::var("CQS_SPLADE_ALPHA") {
        Ok(val) if let Ok(alpha) = val.parse::<f32>() => {
            if alpha.is_finite() {
                let alpha = alpha.clamp(0.0, 1.0);
                // See the per-cat env arm above for why this is debug.
                tracing::debug!(
                    category = %category,
                    alpha,
                    source = "global_env",
                    "SPLADE routing"
                );
                return alpha;
            }
            tracing::warn!(
                var = "CQS_SPLADE_ALPHA",
                value = %val,
                "Non-finite global SPLADE alpha, using default"
            );
        }
        Ok(val) => {
            tracing::warn!(
                var = "CQS_SPLADE_ALPHA",
                value = %val,
                "Invalid global SPLADE alpha (parse error), using default"
            );
        }
        Err(std::env::VarError::NotPresent) => {}
        Err(e) => {
            tracing::warn!(
                var = "CQS_SPLADE_ALPHA",
                error = %e,
                "Global SPLADE alpha env var has non-unicode value — ignored"
            );
        }
    }

    // Per-slot SPLADE α overrides from `slot.toml [splade.alpha]`.
    // Sits between env vars (operator override) and hardcoded defaults
    // (model-category guess) so a slot that's been α-tuned for its
    // embedder doesn't silently inherit values tuned for a different
    // model. Read-locked — write happens once at CLI/daemon startup.
    if let Ok(guard) = SLOT_SPLADE_ALPHA.read() {
        if let Some(table) = guard.as_ref() {
            let key = category.to_string().to_lowercase();
            if let Some(&alpha) = table.get(&key) {
                if alpha.is_finite() {
                    let alpha = alpha.clamp(0.0, 1.0);
                    tracing::debug!(
                        category = %category,
                        alpha,
                        source = "slot_toml",
                        "SPLADE routing"
                    );
                    return alpha;
                }
            }
        }
    }

    // Per-category defaults, sourced from `QueryCategory::default_alpha`
    // (generated by `define_query_categories!` at the top of this file).
    // That match is exhaustive — adding a variant without `default_alpha = ...`
    // is a compile error, so a SPLADE-tuning gap can't slip through under a
    // `_ => 1.0` catch-all. Strategy routing (NameOnly, DenseBase,
    // DenseWithTypeHints) already captures most category-specific behavior,
    // so per-category α mostly matters for queries the router can't strategy-
    // route and for the cross-language fusion tiebreaker.
    let alpha = category.default_alpha();

    // Per-search routing is debug — see the top of `resolve_splade_alpha`.
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
    // Entry span so trace captures show the classifier running on every
    // search query, plus a debug log at exit recording the chosen category /
    // strategy. `classify_query` is pure but on the hot path, so callers
    // often want to confirm the category assignment without recomputing it.
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

/// Lowered query + tokenized words, computed once per query. Passed to each
/// `try_classify_*` so they don't repeat the lowercase + split work.
struct QueryContext<'a> {
    lower: &'a str,
    words: &'a [&'a str],
}

/// Inner body of [`classify_query`] — split so the outer function can log the
/// chosen category once regardless of which branch fires.
///
/// A chain of `try_classify_*(&ctx)` helpers returning `Option<Classification>`.
/// The priority order reads top-to-bottom in the `or_else` chain, and each
/// classifier is independently unit-testable.
fn classify_query_inner(query: &str) -> Classification {
    let query_lower = query.to_lowercase();
    let words: Vec<&str> = query_lower.split_whitespace().collect();
    let ctx = QueryContext {
        lower: &query_lower,
        words: &words,
    };

    if let Some(c) = try_classify_empty(&ctx) {
        return c;
    }
    try_classify_negation(&ctx)
        .or_else(|| try_classify_identifier(&ctx))
        .or_else(|| try_classify_cross_language(&ctx))
        .or_else(|| try_classify_type_filtered(&ctx))
        .or_else(|| try_classify_structural(&ctx))
        .or_else(|| try_classify_behavioral(&ctx))
        .or_else(|| try_classify_conceptual(&ctx))
        .or_else(|| try_classify_multistep(&ctx))
        .unwrap_or(Classification {
            category: QueryCategory::Unknown,
            confidence: Confidence::Low,
            strategy: SearchStrategy::DenseDefault,
            type_hints: None,
        })
}

/// Priority 0: empty query — short-circuits before any other check runs so
/// downstream classifiers can assume `words` is non-empty.
fn try_classify_empty(ctx: &QueryContext) -> Option<Classification> {
    if ctx.words.is_empty() {
        Some(Classification {
            category: QueryCategory::Unknown,
            confidence: Confidence::Low,
            strategy: SearchStrategy::DenseDefault,
            type_hints: None,
        })
    } else {
        None
    }
}

/// Priority 1: Negation trumps everything — "sort without allocating".
/// Routes to the base index because enriched summaries inject positive
/// vocabulary ("allocates", "uses heap") that fights the negation.
///
/// Two-arm context gate: the negation token must function as a CONNECTIVE —
/// either (a) followed by ≥1 non-negation token, OR (b) preceded by a
/// non-negation token. A single-token query that IS a negation word does not
/// qualify, so "no", "exclude", "avoid" used as identifiers/placeholders
/// don't misroute while "sort without allocating" and "behavior except race"
/// still classify as Negation.
fn try_classify_negation(ctx: &QueryContext) -> Option<Classification> {
    if ctx.words.is_empty() {
        return None;
    }
    let g = NEGATION_TOKENS.read().unwrap_or_else(|p| p.into_inner());
    let mut hit_idx = None;
    for (i, w) in ctx.words.iter().enumerate() {
        if g.contains(*w) {
            hit_idx = Some(i);
            break;
        }
    }
    let hit_idx = hit_idx?;

    // Connective context: hit must have a non-negation neighbor on at
    // least one side. A single-word query like just "no" or "avoid" by
    // itself, or a query where the only neighbors are also negation
    // tokens, falls through to the next classifier.
    let has_pre_context = ctx.words.iter().take(hit_idx).any(|w| !g.contains(*w));
    let has_post_context = ctx.words.iter().skip(hit_idx + 1).any(|w| !g.contains(*w));
    if !has_pre_context && !has_post_context {
        return None;
    }

    Some(Classification {
        category: QueryCategory::Negation,
        confidence: Confidence::High,
        strategy: SearchStrategy::DenseBase,
        type_hints: None,
    })
}

/// Priority 2: Identifier lookup — all tokens look like identifiers.
fn try_classify_identifier(ctx: &QueryContext) -> Option<Classification> {
    if is_identifier_query(ctx.lower, ctx.words) {
        Some(Classification {
            category: QueryCategory::IdentifierLookup,
            confidence: Confidence::High,
            strategy: SearchStrategy::NameOnly,
            type_hints: None,
        })
    } else {
        None
    }
}

/// Priority 3: Cross-language — mentions 2+ language names or
/// "equivalent"/"translate". These benefit from the enriched index
/// (summaries add canonical vocabulary that bridges language-specific
/// syntax).
fn try_classify_cross_language(ctx: &QueryContext) -> Option<Classification> {
    if is_cross_language_query(ctx.lower, ctx.words) {
        Some(Classification {
            category: QueryCategory::CrossLanguage,
            confidence: Confidence::High,
            strategy: SearchStrategy::DenseDefault,
            type_hints: None,
        })
    } else {
        None
    }
}

/// Priority 4: Type-filtered — "all structs", "every enum", "test functions".
/// Routes to base: summaries add generic vocabulary that dilutes the specific
/// type signal.
fn try_classify_type_filtered(ctx: &QueryContext) -> Option<Classification> {
    let type_hints = extract_type_hints(ctx.lower);
    if type_hints.is_some() {
        Some(Classification {
            category: QueryCategory::TypeFiltered,
            confidence: Confidence::Medium,
            strategy: SearchStrategy::DenseBase,
            type_hints,
        })
    } else {
        None
    }
}

/// Priority 5: Structural — type keywords + "functions that" patterns.
fn try_classify_structural(ctx: &QueryContext) -> Option<Classification> {
    if is_structural_query(ctx.lower) {
        Some(Classification {
            category: QueryCategory::Structural,
            confidence: Confidence::Medium,
            strategy: SearchStrategy::DenseWithTypeHints,
            type_hints: None,
        })
    } else {
        None
    }
}

/// Priority 6: Behavioral — action verbs, "code that does X".
/// Routes to base: behavioral queries use the verbs the query author chose,
/// and enriched summaries standardize those verbs ("handles" → "processes"),
/// washing out the specific verb the user asked about.
fn try_classify_behavioral(ctx: &QueryContext) -> Option<Classification> {
    if is_behavioral_query(ctx.lower, ctx.words) {
        Some(Classification {
            category: QueryCategory::Behavioral,
            confidence: Confidence::Medium,
            strategy: SearchStrategy::DenseBase,
            type_hints: None,
        })
    } else {
        None
    }
}

/// Priority 7: Conceptual — abstract nouns, short non-identifier queries.
///
/// Routes to the enriched index. Conceptual queries' gold answers are mostly
/// type definitions where the summary helps bridge code → concept ("a service
/// container that resolves dependencies" → "dependency injection"); routing
/// them to the base index strips that signal.
///
/// Note: routing rules are coupled to corpus shape (summary coverage), not to
/// a category-intrinsic property, so re-validate them when summary coverage
/// changes meaningfully.
fn try_classify_conceptual(ctx: &QueryContext) -> Option<Classification> {
    if is_conceptual_query(ctx.lower, ctx.words) {
        Some(Classification {
            category: QueryCategory::Conceptual,
            confidence: Confidence::Medium,
            strategy: SearchStrategy::DenseDefault,
            type_hints: None,
        })
    } else {
        None
    }
}

/// Priority 8: Multi-step — conjunctions.
/// Routes to base: summaries inject vocabulary that displaces the conjunction
/// terms.
fn try_classify_multistep(ctx: &QueryContext) -> Option<Classification> {
    let multistep_hit = {
        let ac = MULTISTEP_PATTERNS_AC
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        ac.is_match(ctx.lower)
    };
    if multistep_hit {
        Some(Classification {
            category: QueryCategory::MultiStep,
            confidence: Confidence::Low,
            strategy: SearchStrategy::DenseBase,
            type_hints: None,
        })
    } else {
        None
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
/// The translation-verb check uses word-token matching for "port" / "ports" /
/// "convert" / "translate" / "equivalent" so it doesn't false-fire on
/// "report", "airport", etc.
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
/// Keyword matching uses whitespace-split word tokens so the keyword fires
/// regardless of position (`"find all trait"`, `"all class"`, `"find enum"`).
fn is_structural_query(query: &str) -> bool {
    // Lowercase here once so callers that pass raw (non-lowercased) query
    // strings still match — a query like `"Class Foo"` would otherwise miss
    // the `class` keyword. The AC pattern set is keyed on lowercase forms.
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
/// The "code that" / "function that" probes use word-boundary checks (not raw
/// substring contains) so hyphenated identifiers like
/// `code-that-was-deleted-yesterday` don't false-fire.
fn is_behavioral_query(query: &str, _words: &[&str]) -> bool {
    if ac_has_word_bounded_match(&BEHAVIORAL_VERBS_AC, query) {
        return true;
    }
    // "code that" / "function that" are specific phrasings used by genuine
    // behavioral queries ("function that embeds a batch of text documents").
    // Broader probes like "how does" / "what does" are deliberately excluded:
    // they catch multi_step queries ("how does X trace callers to find tests")
    // and would misroute them to Behavioral.
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
/// a string boundary or ASCII whitespace. No regex, no allocation, single
/// pass over `query`.
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
/// — needed so sibling patterns like `"constructor"` / `"all constructors"`
/// can both fire — is only valid under the Standard match kind.
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
/// A single Aho-Corasick pass via [`TYPE_HINT_AC`], with the
/// `(phrase, ChunkType)` table built from `ChunkType::hint_phrases()` declared
/// in `define_chunk_types!` — a single source of truth for hint registration.
///
/// Output order follows declaration order: a hint is pushed the first time its
/// pattern id appears, and duplicate `ChunkType`s across different matched
/// patterns are kept (e.g. two Test-mapped patterns both matching yields
/// `[Test, Test]`).
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
// Embedding-space centroid matching: pre-computed category centroids.
// At query time, cosine-sim to each centroid; if top-1 margin over
// top-2 exceeds θ, override the rule-based category. Below θ the
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
        // Cap centroid file reads at 16 MiB. Real centroid registries are
        // 50-200 KiB even with 100 categories × 1024-dim floats; 16 MiB
        // rejects hostile or corrupted multi-GB files before the daemon OOMs.
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
            // Include the path and expected dim so an operator can tell
            // whether the file is at the wrong location, was generated for a
            // different embedding model, or is genuinely empty.
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
/// Enabled by default; set `CQS_CENTROID_CLASSIFIER=0` to opt out. Only fills
/// `Unknown` gaps — a non-Unknown rule-based category is left as-is.
///
/// Call AFTER the query embedding is available.
pub fn reclassify_with_centroid(
    mut classification: Classification,
    embedding: &[f32],
) -> Classification {
    // Entry span so the centroid-upgrade step is visible in traces separately
    // from the outer rule-based classify.
    let _span = tracing::info_span!("reclassify_with_centroid").entered();
    // Fills Unknown gaps with embedding-space classification. Centroid-assigned
    // α is clamped to ≥ CENTROID_ALPHA_FLOOR so a misclassification can't
    // catastrophically zero out SPLADE.
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
            // Per-search event — debug so the operator default log level isn't
            // flooded; the surrounding `info_span!`s carry the trace context.
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
        // Behavioral routes to the base (non-enriched) index because LLM
        // summaries flatten the specific verbs users ask about.
        assert_eq!(c.strategy, SearchStrategy::DenseBase);
    }

    #[test]
    fn test_classify_negation() {
        let c = classify_query("sort without allocating");
        assert_eq!(c.category, QueryCategory::Negation);
        assert_eq!(c.confidence, Confidence::High);
        // Negation routes to base — summaries inject positive vocabulary that
        // fights the "without" clause.
        assert_eq!(c.strategy, SearchStrategy::DenseBase);
    }

    /// Each `try_classify_*` is independently testable — run it with a
    /// hand-built `QueryContext` instead of going through the full priority
    /// chain. Pins the classifier's intrinsic behavior (was identifier?
    /// yes/no) without coupling to whether some earlier arm fires first.
    #[test]
    fn try_classify_helpers_short_circuit_on_match() {
        // Empty context → empty classifier matches, returns Unknown.
        let empty_ctx = QueryContext {
            lower: "",
            words: &[],
        };
        let c = try_classify_empty(&empty_ctx).expect("empty must match");
        assert_eq!(c.category, QueryCategory::Unknown);

        // Identifier classifier returns None on a free-form query.
        let lower = "validates user input".to_string();
        let words: Vec<&str> = lower.split_whitespace().collect();
        let ctx = QueryContext {
            lower: &lower,
            words: &words,
        };
        assert!(
            try_classify_identifier(&ctx).is_none(),
            "identifier classifier must not fire on free-form query"
        );

        // Identifier classifier matches on a snake_case token.
        let lower = "search_filtered".to_string();
        let words: Vec<&str> = lower.split_whitespace().collect();
        let ctx = QueryContext {
            lower: &lower,
            words: &words,
        };
        let c = try_classify_identifier(&ctx).expect("identifier must match snake_case");
        assert_eq!(c.category, QueryCategory::IdentifierLookup);
        assert_eq!(c.strategy, SearchStrategy::NameOnly);
    }

    // ─── classifier vocab overlay ───────────────────────────────────────

    /// An overlay-installed negation token routes a query into the
    /// `Negation` category, just like a builtin would.
    #[test]
    #[serial_test::serial(classifier_vocab_overlay)]
    fn classifier_overlay_negation_token_routes_to_negation_category() {
        reset_classifier_vocab_for_test();
        // "ignoring" isn't in the builtin set; pre-overlay it should NOT
        // hit the negation branch.
        let pre = classify_query("find functions ignoring case");
        assert_ne!(
            pre.category,
            QueryCategory::Negation,
            "ignoring is not a builtin — pre-overlay must not be Negation; got {:?}",
            pre.category
        );

        install_classifier_vocab_overlay(vec!["ignoring".to_string()], Vec::new());

        let post = classify_query("find functions ignoring case");
        assert_eq!(
            post.category,
            QueryCategory::Negation,
            "post-overlay 'ignoring' must route to Negation; got {:?}",
            post.category
        );

        reset_classifier_vocab_for_test();
    }

    /// An overlay-installed multistep pattern routes a query into the
    /// `MultiStep` category.
    #[test]
    #[serial_test::serial(classifier_vocab_overlay)]
    fn classifier_overlay_multistep_pattern_routes_to_multistep_category() {
        reset_classifier_vocab_for_test();
        // "sequentially" isn't in the builtin set.
        let pre = classify_query("walk the tree sequentially");
        assert_ne!(
            pre.category,
            QueryCategory::MultiStep,
            "sequentially is not a builtin — pre-overlay must not be MultiStep; got {:?}",
            pre.category
        );

        install_classifier_vocab_overlay(Vec::new(), vec!["sequentially".to_string()]);

        let post = classify_query("walk the tree sequentially");
        assert_eq!(
            post.category,
            QueryCategory::MultiStep,
            "post-overlay 'sequentially' must route to MultiStep; got {:?}",
            post.category
        );

        reset_classifier_vocab_for_test();
    }

    /// `reset_classifier_vocab_for_test` restores the builtin sets so a
    /// post-reset query lands the same as if no overlay was ever installed.
    #[test]
    #[serial_test::serial(classifier_vocab_overlay)]
    fn classifier_overlay_reset_restores_builtins() {
        reset_classifier_vocab_for_test();
        install_classifier_vocab_overlay(vec!["ignoring".to_string()], Vec::new());
        // confirm the overlay is live
        assert_eq!(
            classify_query("find functions ignoring case").category,
            QueryCategory::Negation
        );
        // reset and confirm the overlay is gone
        reset_classifier_vocab_for_test();
        assert_ne!(
            classify_query("find functions ignoring case").category,
            QueryCategory::Negation,
            "after reset, overlay-only token should no longer trigger Negation"
        );
    }

    /// Empty overlays are no-ops.
    #[test]
    #[serial_test::serial(classifier_vocab_overlay)]
    fn classifier_overlay_empty_is_noop() {
        reset_classifier_vocab_for_test();
        install_classifier_vocab_overlay(Vec::new(), Vec::new());
        // Builtin negation still works — sanity check.
        assert_eq!(
            classify_query("sort without allocating").category,
            QueryCategory::Negation
        );
        reset_classifier_vocab_for_test();
    }

    /// `load_classifier_vocab_overlay` parses the typed `[classifier]` section.
    #[test]
    fn classifier_overlay_loader_parses_typed_section() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("classifier.toml");
        std::fs::write(
            &path,
            "[classifier]\n\
             negation_tokens = [\"ignoring\", \"without using\"]\n\
             multistep_patterns = [\"sequentially\", \"step by step\"]\n",
        )
        .unwrap();

        let (neg, multi) = load_classifier_vocab_overlay(&path);
        assert_eq!(
            neg,
            vec!["ignoring".to_string(), "without using".to_string()]
        );
        assert_eq!(
            multi,
            vec!["sequentially".to_string(), "step by step".to_string()]
        );
    }

    /// Missing file → empty pair (no warn).
    #[test]
    fn classifier_overlay_loader_missing_file_returns_empty() {
        let (neg, multi) =
            load_classifier_vocab_overlay(std::path::Path::new("/nonexistent/classifier.toml"));
        assert!(neg.is_empty());
        assert!(multi.is_empty());
    }

    /// Empty / whitespace-only entries are filtered before reaching
    /// `AhoCorasick::new` (which would panic on `""`).
    #[test]
    fn classifier_overlay_loader_drops_empty_entries() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("classifier.toml");
        std::fs::write(
            &path,
            "[classifier]\n\
             negation_tokens = [\"ignoring\", \"\", \"   \"]\n\
             multistep_patterns = [\"step by step\", \"\"]\n",
        )
        .unwrap();

        let (neg, multi) = load_classifier_vocab_overlay(&path);
        assert_eq!(neg, vec!["ignoring".to_string()]);
        assert_eq!(multi, vec!["step by step".to_string()]);
    }

    #[test]
    fn test_classify_conceptual_routes_to_enriched() {
        // Conceptual routes to the enriched index: the summary bridges
        // code → concept on struct/enum chunks, which is where conceptual
        // queries' gold answers live.
        let c = classify_query("dependency injection pattern");
        assert_eq!(c.category, QueryCategory::Conceptual);
        assert_eq!(c.strategy, SearchStrategy::DenseDefault);
    }

    #[test]
    fn test_classify_structural_stays_on_enriched() {
        // Structural queries benefit from enrichment, so they use the
        // DenseWithTypeHints (enriched HNSW) strategy.
        let c = classify_query("functions that return Result");
        assert_eq!(c.category, QueryCategory::Structural);
        assert_eq!(c.strategy, SearchStrategy::DenseWithTypeHints);
    }

    #[test]
    fn test_classify_cross_language_stays_on_enriched() {
        // Cross-language queries rely on canonical vocabulary that summaries
        // provide, so they stay on enriched.
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

    /// Structural keywords at end-of-query must classify as Structural.
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

    /// Structural keywords as substrings of normal words ("training" contains
    /// "trait") must NOT false-fire structural.
    #[test]
    fn test_p2_44_structural_keyword_as_substring_does_not_match() {
        assert!(!is_structural_query("training pipeline"));
        assert!(!is_structural_query("classifier"));
    }

    /// `is_structural_query` case-folds, so callers that pass raw queries
    /// (`is_conceptual_query`, the centroid path, external test callers)
    /// classify uppercase or mixed-case structural keywords correctly.
    /// Note: `STRUCTURAL_KEYWORDS` is singular-only (`struct`, not `structs`);
    /// this pins the uppercase axis only.
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
        // type_filtered routes to base — summaries dilute the type signal.
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
        // multi_step routes to base — summaries displace conjunction terms.
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

    /// The connective-context gate must let real
    /// multi-token negations through ("sort without allocating",
    /// "behavior except race", "fn that doesn't panic").
    #[test]
    fn test_classify_negation_connective_context_passes() {
        for query in [
            "sort without allocating",
            "behavior except race",
            "fn that doesn't panic",
            "find code without locks",
            "search avoid contention path",
        ] {
            let c = classify_query(query);
            assert_eq!(
                c.category,
                QueryCategory::Negation,
                "real negation `{query}` must classify as Negation"
            );
        }
    }

    /// Bare common nouns that happen to be negation
    /// tokens must NOT classify as Negation. `cqs "exclude"`, `cqs "no"`,
    /// `cqs "avoid"` as single-token queries are placeholder/identifier
    /// queries, not negations.
    #[test]
    fn test_classify_bare_negation_token_falls_through() {
        for query in ["exclude", "avoid", "no", "without", "except"] {
            let c = classify_query(query);
            assert_ne!(
                c.category,
                QueryCategory::Negation,
                "single-token negation word `{query}` must NOT classify as Negation"
            );
        }
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

    // ── MultiStep pattern tightening ─────────────────────────────────

    #[test]
    fn test_classify_plain_and_is_not_multistep() {
        // "find foo and bar" is a single search intent, not a multi-step query.
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

    // ── cross-language classifier word-boundary ─────────────────────

    #[test]
    fn test_classify_report_is_not_cross_language() {
        // "report" must not trigger cross-language via a "port" substring match.
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

    // ── behavioral classifier word-boundary ─────────────────────────

    #[test]
    fn test_classify_word_bounded_code_that_not_behavioral() {
        // A token-attached "code that" like `barcode that1` contains the
        // literal "code that" substring but is not the phrase "code that"
        // as a word, so the word-boundary check must not fire Behavioral.
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

    // ── structural keyword end-of-query ─────────────────────────────

    #[test]
    fn test_structural_keyword_at_end_of_query() {
        // Keywords at any position — including end-of-query — match.
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
        // A single-keyword query (just "trait") is structural.
        assert!(is_structural_query("trait"));
        assert!(is_structural_query("struct"));
    }

    #[test]
    fn test_structural_keyword_middle_still_works() {
        // Keyword in the middle of a query matches.
        assert!(is_structural_query("find all trait implementations"));
        assert!(is_structural_query("every class definition"));
    }

    // ── per-slot SPLADE α overrides ───────────────────────────────────────

    /// Slot table installed → that override beats the hardcoded default for
    /// the matching category.
    #[test]
    #[serial_test::serial(splade_alpha_state)]
    fn slot_splade_alpha_override_wins_over_default() {
        clear_slot_splade_alpha_overrides();
        // Clear env so the test doesn't accidentally pick up an outer
        // `CQS_SPLADE_ALPHA_*` value.
        std::env::remove_var("CQS_SPLADE_ALPHA");
        std::env::remove_var(format!(
            "CQS_SPLADE_ALPHA_{}",
            QueryCategory::Behavioral.to_string().to_uppercase()
        ));

        let mut table = std::collections::HashMap::new();
        // Default for Behavioral is 1.0. Override to 0.3.
        table.insert("behavioral".to_string(), 0.3);
        install_slot_splade_alpha_overrides(table);

        let got = resolve_splade_alpha(&QueryCategory::Behavioral);
        assert!(
            (got - 0.3).abs() < f32::EPSILON,
            "slot α=0.3 must beat default 1.0; got {got}"
        );

        clear_slot_splade_alpha_overrides();
    }

    /// Env vars beat slot table — operator override is highest precedence.
    #[test]
    #[serial_test::serial(splade_alpha_state)]
    fn env_splade_alpha_beats_slot_override() {
        clear_slot_splade_alpha_overrides();
        let mut table = std::collections::HashMap::new();
        table.insert("conceptual".to_string(), 0.1);
        install_slot_splade_alpha_overrides(table);

        // Per-cat env override.
        std::env::set_var("CQS_SPLADE_ALPHA_CONCEPTUAL", "0.55");
        let got = resolve_splade_alpha(&QueryCategory::Conceptual);
        assert!(
            (got - 0.55).abs() < f32::EPSILON,
            "per-cat env α=0.55 must beat slot α=0.1; got {got}"
        );

        std::env::remove_var("CQS_SPLADE_ALPHA_CONCEPTUAL");

        // Global env override.
        std::env::set_var("CQS_SPLADE_ALPHA", "0.42");
        let got = resolve_splade_alpha(&QueryCategory::Conceptual);
        assert!(
            (got - 0.42).abs() < f32::EPSILON,
            "global env α=0.42 must beat slot α=0.1; got {got}"
        );
        std::env::remove_var("CQS_SPLADE_ALPHA");
        clear_slot_splade_alpha_overrides();
    }

    /// Slot table without a key for the category → falls through to the default.
    #[test]
    #[serial_test::serial(splade_alpha_state)]
    fn slot_splade_alpha_partial_table_falls_through_to_default() {
        clear_slot_splade_alpha_overrides();
        std::env::remove_var("CQS_SPLADE_ALPHA");
        std::env::remove_var(format!(
            "CQS_SPLADE_ALPHA_{}",
            QueryCategory::Behavioral.to_string().to_uppercase()
        ));

        let mut table = std::collections::HashMap::new();
        // Only `unknown` defined; Behavioral falls through.
        table.insert("unknown".to_string(), 0.3);
        install_slot_splade_alpha_overrides(table);

        let got = resolve_splade_alpha(&QueryCategory::Behavioral);
        let default = QueryCategory::Behavioral.default_alpha();
        assert!(
            (got - default).abs() < f32::EPSILON,
            "Behavioral not in slot table → uses default α={default}; got {got}"
        );

        clear_slot_splade_alpha_overrides();
    }

    #[test]
    fn test_structural_keyword_substring_does_not_fire() {
        // Word-split matching: "MyTraitImpl" as a CamelCase identifier does
        // NOT classify as structural just because it contains "trait".
        assert!(!is_structural_query("MyTraitImpl"));
    }

    // ── Micro-benchmark ──────────────────────────────────────────────
    //
    // Runs classify_query on a mix of query shapes and prints per-call
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
        //   1. Type-filtered — runs the full extract_type_hints table.
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
