//! Kind detection for polymorphic command routing.
//!
//! Given a name string, classify it against the indexed corpus by querying
//! the chunks table for exact matches, then grouping by the high-level
//! `Kind` (Function | Type | Const | Module | Other). See
//! `docs/polymorphic-routing.md` for the per-(command × kind) behavior matrix.
//!
//! This is a lib building block; no CLI command is rerouted through it yet.
//!
//! ## Why a separate Kind enum (vs. reusing ChunkType / ChunkClass)
//!
//! `ChunkType` (24 variants) is too granular for routing decisions —
//! every variant would have to enumerate its dispatch behavior.
//! `ChunkClass` (Callable / Code / NonCode) is too coarse — it
//! collapses Type and Const together, but routing wants those
//! distinct (a const has a value, a type has methods).
//!
//! `Kind` is the routing-level grouping the design doc specifies:
//! - `Function`: callable that participates in the call graph.
//! - `Type`: nominal definition (Class / Struct / Enum / Trait / etc.).
//! - `Const`: value-carrying definition (Constant / Variable).
//! - `Module`: namespace / module-scope definition.
//! - `Other`: chunk types that don't fit the routing matrix yet
//!   (Macro, Impl, ConfigKey, StoredProc, etc.) — treated as
//!   freeform-shaped today; future expansion happens here.
//!
//! ## Kind vs. KindResolution
//!
//! `Kind` classifies a *single* chunk. Resolving a *set* of name-match
//! hits also has aggregate outcomes (zero matches, several of one kind,
//! several mixed kinds) that aren't groupings of any one chunk. Those live
//! in [`KindResolution`] — `Resolved(Kind)` for the single-kind case plus
//! `Multiple` / `Ambiguous` / `NotFound`. Keeping them in separate types
//! means `classify_chunk_type`'s callers (`routing_priority`, the store's
//! ORDER BY) match over five honest routing arms instead of carrying dead
//! aggregate cases an individual chunk can never reach.

use crate::language::{ChunkType, Language};
use crate::store::{ChunkSummary, Store, StoreError};
use std::collections::HashSet;
use std::path::PathBuf;

/// Routing-level grouping for a single chunk's classification.
///
/// These are the kinds a *single* `ChunkType` maps to — the groupings the
/// per-(command × kind) routing matrix dispatches on. Aggregate outcomes
/// (a name matching zero, several same-kind, or several mixed-kind chunks)
/// live in [`KindResolution`], not here: this enum only ever describes one
/// concrete definition, so an exhaustive `match Kind` downstream sees five
/// honest routing arms with no dead aggregate cases to enumerate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// Callable — `ChunkType::{Function, Method, Constructor, Test, Endpoint, Middleware}`.
    Function,
    /// Nominal type definition — `ChunkType::{Class, Struct, Enum, Trait, Interface, TypeAlias, Object, Delegate}`.
    Type,
    /// Value-carrying definition — `ChunkType::{Constant, Variable, Property, Event}`.
    Const,
    /// Namespace / module-scope — `ChunkType::{Module, Namespace}`.
    Module,
    /// Anything else (Macro, Impl, ConfigKey, StoredProc, Extern,
    /// Modifier, Section, Service). Routing-level callers should
    /// treat these as freeform results today; future fallback rules
    /// land here as the design matrix expands.
    Other,
}

/// Outcome of resolving a *set* of name-match hits to a routing decision.
///
/// Split out from [`Kind`] (API-V1.40-2): the old combined enum mixed the
/// five single-chunk routing kinds with three set-level dispatch outcomes,
/// so every exhaustive `match` downstream had to enumerate aggregate arms
/// that `classify_chunk_type` could never produce. This type owns the
/// aggregate decisions; [`Kind`] owns the per-chunk grouping.
///
/// - [`Resolved`](Self::Resolved) wraps the single [`Kind`] when exactly one
///   hit matched. The happy path: the command either runs its normal flow
///   or a single-kind fallback.
/// - The three remaining variants are the set-level outcomes the
///   polymorphic-routing doc drives fallback behavior from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KindResolution {
    /// Exactly one hit matched, surfaced as its single routing [`Kind`].
    Resolved(Kind),
    /// Multiple kinds match the same name (e.g. `len` is both a method
    /// AND a const in some codebases). Caller should surface all
    /// candidates with kind labels.
    Ambiguous,
    /// Multiple matches of the same kind (e.g. a function defined in
    /// several files / languages). Caller should surface all candidates.
    Multiple,
    /// No exact name match in the index. Caller should fall through
    /// to freeform search.
    NotFound,
}

/// One hit from `detect_kind`. Carries the location info the routing
/// matrix needs to decide which command to dispatch (or whether to
/// fall back to a freeform search).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KindHit {
    pub chunk_type: ChunkType,
    pub file: PathBuf,
    pub line_start: u32,
    pub language: Language,
    pub name: String,
}

impl From<&ChunkSummary> for KindHit {
    fn from(c: &ChunkSummary) -> Self {
        Self {
            chunk_type: c.chunk_type,
            file: c.file.clone(),
            line_start: c.line_start,
            language: c.language,
            name: c.name.clone(),
        }
    }
}

/// Classify a single `ChunkType` into its routing-level `Kind`.
///
/// The match is exhaustive: adding a new `ChunkType` variant produces
/// a **compile error** here, forcing the author to either add it to an
/// existing `Kind` arm or to the `Kind::Other` catch-all. There is no
/// runtime fallback (and no `tracing::warn` — the type system handles
/// the reminder more strictly than logging would).
pub fn classify_chunk_type(ct: ChunkType) -> Kind {
    match ct {
        ChunkType::Function
        | ChunkType::Method
        | ChunkType::Constructor
        | ChunkType::Test
        | ChunkType::Endpoint
        | ChunkType::Middleware => Kind::Function,
        ChunkType::Class
        | ChunkType::Struct
        | ChunkType::Enum
        | ChunkType::Trait
        | ChunkType::Interface
        | ChunkType::TypeAlias
        | ChunkType::Object
        | ChunkType::Delegate => Kind::Type,
        ChunkType::Constant | ChunkType::Variable | ChunkType::Property | ChunkType::Event => {
            Kind::Const
        }
        ChunkType::Module | ChunkType::Namespace => Kind::Module,
        // Catch-all: future variants (and existing ones the routing
        // matrix doesn't yet rule on) land here.
        ChunkType::Section
        | ChunkType::Macro
        | ChunkType::Impl
        | ChunkType::ConfigKey
        | ChunkType::Service
        | ChunkType::StoredProc
        | ChunkType::Extern
        | ChunkType::Modifier
        | ChunkType::Extension => Kind::Other,
    }
}

/// Routing priority for a [`Kind`]: lower values sort first in
/// `Store::get_chunks_by_name`'s ORDER BY.
///
/// The classifier ([`classify_hits`]) reduces over the *set* of kinds it
/// sees, so ordering only becomes load-bearing once the lookup is bounded:
/// for a hot name with more matches than the row cap, the priority decides
/// which kinds survive the cut and therefore how the name classifies.
/// Callables rank first because every graph command's happy path routes on
/// `Kind::Function` — a callable name that also collides with hundreds of
/// alphabetically-earlier types or consts must still present its callable
/// evidence to the classifier, or the command would misroute into a
/// wrong-kind fallback. Types outrank consts (deps runs its normal flow
/// for types), and modules come last among the routed kinds.
pub fn routing_priority(kind: Kind) -> u8 {
    // Exhaustive over the five routing kinds — the aggregate dispatch
    // outcomes live in `KindResolution` now, so there are no dead arms to
    // enumerate here. A new `Kind` variant is a compile error until it
    // picks a rank.
    match kind {
        Kind::Function => 0,
        Kind::Type => 1,
        Kind::Const => 2,
        Kind::Module => 3,
        Kind::Other => 4,
    }
}

/// One-shot kind detection: query the store for exact-name matches **once**,
/// classify them, and return the [`KindResolution`] alongside the full
/// [`ChunkSummary`] rows that produced it.
///
/// Returning the summaries (not the lossy [`KindHit`] projection) is the
/// DS-V1.40-8/10 fix: a dispatcher that decides to render a kind-mismatch
/// fallback reuses *these* rows for its `definitions[]` rather than issuing
/// a second `WHERE name = ?` query. One read feeds both the routing
/// decision and the rendering, so the two can't disagree under a concurrent
/// reindex (no snapshot drift between "what kind is this" and "what are its
/// definitions").
///
/// A command dispatcher calls this to decide whether to run the original
/// handler (kind matches the command) or fall back to a kind-labeled
/// response (kind mismatch).
///
/// Cost: one indexed SQL query (`WHERE name = ?`) — not measurable
/// against the surrounding command latency for interactive use.
pub fn detect_kind_for_store<Mode>(
    store: &Store<Mode>,
    name: &str,
) -> Result<(KindResolution, Vec<ChunkSummary>), StoreError> {
    let _span = tracing::info_span!("detect_kind_for_store", %name).entered();
    let chunks = store.get_chunks_by_name(name)?;
    let hits: Vec<KindHit> = chunks.iter().map(KindHit::from).collect();
    let resolution = classify_hits(&hits);
    Ok((resolution, chunks))
}

/// Reduce a sequence of hits to a single [`KindResolution`] decision.
///
/// `hits` should be the exact-name-match results from the chunks
/// table. The classifier:
/// - 0 hits → `NotFound`
/// - 1 hit  → `Resolved(kind)` (Function / Type / Const / Module / Other)
/// - N hits, all same Kind → `Multiple`
/// - N hits, mixed Kinds → `Ambiguous`
pub fn classify_hits(hits: &[KindHit]) -> KindResolution {
    let _span = tracing::info_span!("classify_hits", hits = hits.len()).entered();
    if hits.is_empty() {
        return KindResolution::NotFound;
    }
    let kinds: HashSet<Kind> = hits
        .iter()
        .map(|h| classify_chunk_type(h.chunk_type))
        .collect();
    if kinds.len() > 1 {
        return KindResolution::Ambiguous;
    }
    if hits.len() > 1 {
        return KindResolution::Multiple;
    }
    // Logically `kinds.len() == 1` here (the early-returns above
    // ensure non-empty + non-mixed). `unwrap_or` keeps the function
    // panic-free per the project's "no unwrap outside tests" rule —
    // a future refactor that breaks the invariant will route to
    // `Kind::Other` (the routing-level catch-all) rather than crash.
    KindResolution::Resolved(kinds.into_iter().next().unwrap_or(Kind::Other))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(chunk_type: ChunkType, name: &str, file: &str, line: u32) -> KindHit {
        KindHit {
            chunk_type,
            file: PathBuf::from(file),
            line_start: line,
            language: Language::Rust,
            name: name.to_string(),
        }
    }

    #[test]
    fn classify_chunk_type_function_family() {
        assert_eq!(classify_chunk_type(ChunkType::Function), Kind::Function);
        assert_eq!(classify_chunk_type(ChunkType::Method), Kind::Function);
        assert_eq!(classify_chunk_type(ChunkType::Constructor), Kind::Function);
        assert_eq!(classify_chunk_type(ChunkType::Test), Kind::Function);
    }

    #[test]
    fn classify_chunk_type_type_family() {
        assert_eq!(classify_chunk_type(ChunkType::Class), Kind::Type);
        assert_eq!(classify_chunk_type(ChunkType::Struct), Kind::Type);
        assert_eq!(classify_chunk_type(ChunkType::Enum), Kind::Type);
        assert_eq!(classify_chunk_type(ChunkType::Trait), Kind::Type);
        assert_eq!(classify_chunk_type(ChunkType::Interface), Kind::Type);
        assert_eq!(classify_chunk_type(ChunkType::TypeAlias), Kind::Type);
    }

    #[test]
    fn classify_chunk_type_const_family() {
        assert_eq!(classify_chunk_type(ChunkType::Constant), Kind::Const);
        assert_eq!(classify_chunk_type(ChunkType::Variable), Kind::Const);
    }

    #[test]
    fn classify_chunk_type_module_family() {
        assert_eq!(classify_chunk_type(ChunkType::Module), Kind::Module);
        assert_eq!(classify_chunk_type(ChunkType::Namespace), Kind::Module);
    }

    #[test]
    fn classify_chunk_type_other_family() {
        assert_eq!(classify_chunk_type(ChunkType::Macro), Kind::Other);
        assert_eq!(classify_chunk_type(ChunkType::Impl), Kind::Other);
        assert_eq!(classify_chunk_type(ChunkType::ConfigKey), Kind::Other);
        assert_eq!(classify_chunk_type(ChunkType::Section), Kind::Other);
    }

    /// Table-driven pin over every [`ChunkType`] variant. The per-family
    /// tests above only exercise a representative subset (13 of 24
    /// variants); this iterates `ChunkType::ALL` so a newly-added variant
    /// (or a re-bucketed existing one — Endpoint/Middleware → Function,
    /// Object/Delegate → Type, Property/Event → Const, Service/StoredProc/
    /// Extern/Modifier/Extension → Other) can't slip through unclassified.
    /// `classify_chunk_type`'s match is already exhaustive (a new variant is
    /// a compile error there), but this pins the *routing intent* of each
    /// variant, which the compiler can't check.
    #[test]
    fn classify_chunk_type_covers_every_variant() {
        fn expected(ct: ChunkType) -> Kind {
            match ct {
                ChunkType::Function
                | ChunkType::Method
                | ChunkType::Constructor
                | ChunkType::Test
                | ChunkType::Endpoint
                | ChunkType::Middleware => Kind::Function,
                ChunkType::Class
                | ChunkType::Struct
                | ChunkType::Enum
                | ChunkType::Trait
                | ChunkType::Interface
                | ChunkType::TypeAlias
                | ChunkType::Object
                | ChunkType::Delegate => Kind::Type,
                ChunkType::Constant
                | ChunkType::Variable
                | ChunkType::Property
                | ChunkType::Event => Kind::Const,
                ChunkType::Module | ChunkType::Namespace => Kind::Module,
                ChunkType::Section
                | ChunkType::Macro
                | ChunkType::Impl
                | ChunkType::ConfigKey
                | ChunkType::Service
                | ChunkType::StoredProc
                | ChunkType::Extern
                | ChunkType::Modifier
                | ChunkType::Extension => Kind::Other,
            }
        }

        // Every variant classifies; none falls through to a panic, and the
        // result is one of the routing-level groupings.
        for &ct in ChunkType::ALL {
            let got = classify_chunk_type(ct);
            assert_eq!(
                got,
                expected(ct),
                "classify_chunk_type({ct:?}) routing intent drifted"
            );
            // `classify_chunk_type` returns a routing `Kind`, never an
            // aggregate outcome — those live in `KindResolution` now, so
            // the type system already rules them out here. (The aggregate
            // path is exercised by the `classify_hits_*` tests below.)
        }

        // Sanity: the previously-undertested variants are present in ALL and
        // land in the buckets the doc comment promises.
        assert_eq!(classify_chunk_type(ChunkType::Endpoint), Kind::Function);
        assert_eq!(classify_chunk_type(ChunkType::Middleware), Kind::Function);
        assert_eq!(classify_chunk_type(ChunkType::Object), Kind::Type);
        assert_eq!(classify_chunk_type(ChunkType::Delegate), Kind::Type);
        assert_eq!(classify_chunk_type(ChunkType::Property), Kind::Const);
        assert_eq!(classify_chunk_type(ChunkType::Event), Kind::Const);
        assert_eq!(classify_chunk_type(ChunkType::Service), Kind::Other);
        assert_eq!(classify_chunk_type(ChunkType::StoredProc), Kind::Other);
        assert_eq!(classify_chunk_type(ChunkType::Extern), Kind::Other);
        assert_eq!(classify_chunk_type(ChunkType::Modifier), Kind::Other);
        assert_eq!(classify_chunk_type(ChunkType::Extension), Kind::Other);
    }

    #[test]
    fn classify_hits_empty_is_not_found() {
        assert_eq!(classify_hits(&[]), KindResolution::NotFound);
    }

    #[test]
    fn classify_hits_single_function_returns_function() {
        let hits = vec![hit(ChunkType::Function, "foo", "src/a.rs", 10)];
        assert_eq!(
            classify_hits(&hits),
            KindResolution::Resolved(Kind::Function)
        );
    }

    #[test]
    fn classify_hits_single_const_returns_const() {
        let hits = vec![hit(ChunkType::Constant, "FOO", "src/a.rs", 5)];
        assert_eq!(classify_hits(&hits), KindResolution::Resolved(Kind::Const));
    }

    #[test]
    fn classify_hits_two_same_kind_is_multiple() {
        let hits = vec![
            hit(ChunkType::Function, "foo", "src/a.rs", 10),
            hit(ChunkType::Function, "foo", "src/b.rs", 20),
        ];
        assert_eq!(classify_hits(&hits), KindResolution::Multiple);
    }

    #[test]
    fn classify_hits_two_kinds_is_ambiguous() {
        let hits = vec![
            hit(ChunkType::Method, "len", "src/a.rs", 10),
            hit(ChunkType::Constant, "len", "src/b.rs", 5),
        ];
        assert_eq!(classify_hits(&hits), KindResolution::Ambiguous);
    }

    #[test]
    fn classify_hits_method_and_function_collapses_to_function_multiple() {
        // Method and Function are both `Kind::Function` — this should
        // surface as `Multiple` (same kind, different chunks), not
        // `Ambiguous` (different kinds). Pin the contract.
        let hits = vec![
            hit(ChunkType::Function, "process", "src/a.rs", 10),
            hit(ChunkType::Method, "process", "src/b.rs", 20),
        ];
        assert_eq!(classify_hits(&hits), KindResolution::Multiple);
    }

    fn build_test_chunk(name: &str, file: &str) -> crate::parser::Chunk {
        let content = format!("fn {}() {{ /* body */ }}", name);
        let hash = blake3::hash(content.as_bytes()).to_hex().to_string();
        crate::parser::Chunk {
            id: format!("{}:1:{}", file, &hash[..8]),
            file: PathBuf::from(file),
            language: Language::Rust,
            chunk_type: ChunkType::Function,
            name: name.to_string(),
            signature: format!("fn {}()", name),
            content,
            doc: None,
            line_start: 1,
            line_end: 5,
            content_hash: hash,
            canonical_hash: String::new(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        }
    }

    #[test]
    fn detect_kind_for_store_round_trip_function() {
        // End-to-end: seed a single Function chunk into a fresh store,
        // detect_kind should classify it as Kind::Function and return
        // exactly one hit.
        use crate::store::Store;
        use crate::test_helpers::mock_embedding;

        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join(crate::INDEX_DB_FILENAME);
        let store = Store::open(&db_path).unwrap();
        store.init(&crate::store::ModelInfo::default()).unwrap();
        let chunk = build_test_chunk("foo", "src/lib.rs");
        store
            .upsert_chunks_batch(&[(chunk, mock_embedding(1.0))], Some(100))
            .unwrap();

        let (resolution, hits) = detect_kind_for_store(&store, "foo").unwrap();
        assert_eq!(resolution, KindResolution::Resolved(Kind::Function));
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "foo");
    }

    #[test]
    fn detect_kind_for_store_returns_not_found_for_missing_name() {
        // No seeding — the store is empty. A name lookup returns
        // NotFound with an empty hits vector.
        use crate::store::Store;

        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join(crate::INDEX_DB_FILENAME);
        let store = Store::open(&db_path).unwrap();
        store.init(&crate::store::ModelInfo::default()).unwrap();

        let (resolution, hits) = detect_kind_for_store(&store, "missing_name").unwrap();
        assert_eq!(resolution, KindResolution::NotFound);
        assert!(hits.is_empty());
    }

    #[test]
    fn detect_kind_for_store_cross_kind_collision_still_ambiguous() {
        // The bounded, priority-ordered lookup must not change how a
        // routing-relevant collision classifies: a name that is both a
        // function and a const (well under the row cap) stays Ambiguous,
        // exactly as it did with the unbounded alphabetical lookup.
        use crate::store::Store;
        use crate::test_helpers::mock_embedding;

        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join(crate::INDEX_DB_FILENAME);
        let store = Store::open(&db_path).unwrap();
        store.init(&crate::store::ModelInfo::default()).unwrap();

        let fn_chunk = build_test_chunk("len", "src/a.rs");
        let mut const_chunk = build_test_chunk("len", "src/b.rs");
        const_chunk.chunk_type = ChunkType::Constant;
        const_chunk.id = "src/b.rs:1:constlen".to_string();
        store
            .upsert_chunks_batch(
                &[
                    (fn_chunk, mock_embedding(1.0)),
                    (const_chunk, mock_embedding(2.0)),
                ],
                Some(100),
            )
            .unwrap();

        let (resolution, hits) = detect_kind_for_store(&store, "len").unwrap();
        assert_eq!(resolution, KindResolution::Ambiguous);
        assert_eq!(hits.len(), 2);
        // Priority order: the callable hit ranks first.
        assert_eq!(hits[0].chunk_type, ChunkType::Function);
    }

    #[test]
    fn routing_priority_callables_first() {
        // Pin the relative ranking the ORDER BY depends on.
        assert!(routing_priority(Kind::Function) < routing_priority(Kind::Type));
        assert!(routing_priority(Kind::Type) < routing_priority(Kind::Const));
        assert!(routing_priority(Kind::Const) < routing_priority(Kind::Module));
        assert!(routing_priority(Kind::Module) < routing_priority(Kind::Other));
    }

    #[test]
    fn kind_hit_from_chunk_summary_round_trip() {
        // The `From<&ChunkSummary>` impl is the production constructor;
        // test it round-trips a representative summary cleanly. Manual
        // fixture (no Store needed) so this stays a pure unit test.
        let summary = ChunkSummary {
            id: "id-1".into(),
            file: PathBuf::from("src/a.rs"),
            language: Language::Rust,
            chunk_type: ChunkType::Function,
            name: "foo".into(),
            signature: "fn foo()".into(),
            content: "fn foo() {}".into(),
            doc: None,
            line_start: 10,
            line_end: 12,
            content_hash: String::new(),
            window_idx: None,
            parent_id: None,
            parent_type_name: None,
            parser_version: 0,
            vendored: false,
        };
        let hit = KindHit::from(&summary);
        assert_eq!(hit.name, "foo");
        assert_eq!(hit.chunk_type, ChunkType::Function);
        assert_eq!(hit.line_start, 10);
        assert_eq!(hit.file, PathBuf::from("src/a.rs"));
    }
}
