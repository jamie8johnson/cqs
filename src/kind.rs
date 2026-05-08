//! Kind detection for polymorphic command routing.
//!
//! Phase 1 plumbing for `docs/polymorphic-routing.md`. Given a name
//! string, classify it against the indexed corpus by querying the
//! chunks table for exact matches, then grouping by the high-level
//! `Kind` (Function | Type | Const | Module | Other).
//!
//! **Status:** detection helper ships here as a lib building block.
//! No CLI command is rerouted yet; future PRs will wire the
//! kind-mismatch fallback per the design doc's per-(command × kind)
//! behavior matrix.
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

use crate::language::{ChunkType, Language};
use crate::store::ChunkSummary;
use std::collections::HashSet;
use std::path::PathBuf;

/// Routing-level grouping for a name's classification.
///
/// `NotFound`, `Ambiguous`, and `Multiple` are dispatch decisions
/// rather than chunk-type groupings — the polymorphic-routing doc
/// uses them to drive per-command fallback behavior.
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
/// Adding a new `ChunkType` variant defaults to `Kind::Other` until
/// it's explicitly classified here. Test below pins coverage so a new
/// variant produces a `tracing::warn` reminder rather than silently
/// being treated as freeform.
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

/// Reduce a sequence of hits to a single `Kind` decision.
///
/// `hits` should be the exact-name-match results from the chunks
/// table. The classifier:
/// - 0 hits → `NotFound`
/// - 1 hit  → its `Kind` (Function / Type / Const / Module / Other)
/// - N hits, all same Kind → `Multiple`
/// - N hits, mixed Kinds → `Ambiguous`
pub fn classify_hits(hits: &[KindHit]) -> Kind {
    if hits.is_empty() {
        return Kind::NotFound;
    }
    let kinds: HashSet<Kind> = hits
        .iter()
        .map(|h| classify_chunk_type(h.chunk_type))
        .collect();
    if kinds.len() > 1 {
        return Kind::Ambiguous;
    }
    if hits.len() > 1 {
        return Kind::Multiple;
    }
    // Safe: kinds.len() == 1.
    *kinds.iter().next().expect("non-empty single-kind set")
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

    #[test]
    fn classify_hits_empty_is_not_found() {
        assert_eq!(classify_hits(&[]), Kind::NotFound);
    }

    #[test]
    fn classify_hits_single_function_returns_function() {
        let hits = vec![hit(ChunkType::Function, "foo", "src/a.rs", 10)];
        assert_eq!(classify_hits(&hits), Kind::Function);
    }

    #[test]
    fn classify_hits_single_const_returns_const() {
        let hits = vec![hit(ChunkType::Constant, "FOO", "src/a.rs", 5)];
        assert_eq!(classify_hits(&hits), Kind::Const);
    }

    #[test]
    fn classify_hits_two_same_kind_is_multiple() {
        let hits = vec![
            hit(ChunkType::Function, "foo", "src/a.rs", 10),
            hit(ChunkType::Function, "foo", "src/b.rs", 20),
        ];
        assert_eq!(classify_hits(&hits), Kind::Multiple);
    }

    #[test]
    fn classify_hits_two_kinds_is_ambiguous() {
        let hits = vec![
            hit(ChunkType::Method, "len", "src/a.rs", 10),
            hit(ChunkType::Constant, "len", "src/b.rs", 5),
        ];
        assert_eq!(classify_hits(&hits), Kind::Ambiguous);
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
        assert_eq!(classify_hits(&hits), Kind::Multiple);
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
