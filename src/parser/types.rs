//! Data types for the parser module

use serde::Serialize;
use std::path::PathBuf;
use thiserror::Error;

// Re-export from language module (source of truth)
pub use crate::language::{
    capture_name_to_chunk_type, ChunkType, FieldStyle, Language, SignatureStyle,
};

/// Errors that can occur during code parsing
#[derive(Error, Debug)]
pub enum ParserError {
    /// File extension not recognized as a supported language
    #[error("Unsupported file type: {0}")]
    UnsupportedFileType(String),
    /// Tree-sitter failed to parse the file contents
    #[error("Failed to parse: {0}")]
    ParseFailed(String),
    /// Tree-sitter query compilation failed (indicates bug in query string)
    #[error("Failed to compile query for {0}: {1}")]
    QueryCompileFailed(String, String),
    /// File read error
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// A parsed code chunk (function, method, class, etc.)
/// Chunks are the basic unit of indexing and search in cqs.
/// Each chunk represents a single code element extracted by tree-sitter.
#[derive(Debug, Clone)]
pub struct Chunk {
    /// Unique identifier: `{file}:{line_start}:{byte_start}:{hash8}` (see
    /// `parser::chunk::chunk_id`), with a structural suffix for chunks that
    /// legitimately share those base coordinates with a sibling (see
    /// `parser::chunk::chunk_id_suffixed`): `…:t{idx}` for a markdown table,
    /// `…:t{idx}w{widx}` for a table window, and `{parent_id}:w{window_idx}` for
    /// a token-window code chunk.
    pub id: String,
    /// Source file path (typically absolute during indexing, stored as provided)
    pub file: PathBuf,
    /// Programming language
    pub language: Language,
    /// Type of code element
    pub chunk_type: ChunkType,
    /// Name of the function/class/etc.
    pub name: String,
    /// Function signature or declaration line
    pub signature: String,
    /// Full source code content (may be windowed portion of original)
    pub content: String,
    /// Documentation comment if present
    pub doc: Option<String>,
    /// Starting line number (1-indexed)
    pub line_start: u32,
    /// Ending line number (1-indexed)
    pub line_end: u32,
    /// Start byte offset of this chunk's source element within its file
    /// (0-indexed). The id disambiguator: byte-distinct chunks of one file
    /// occupy disjoint byte ranges, so this is unique per chunk within a file
    /// even when `line_start` and `content_hash` collide (same-line macro twins,
    /// `struct A; impl A {}`). Deterministic and stable across re-runs of the
    /// same source; folded into the id by `parser::chunk::chunk_id`. Hydrated-
    /// from-DB chunks set this to 0 (never consulted off the freshly-parsed
    /// path; the id is already final).
    pub byte_start: u32,
    /// BLAKE3 hash of content for change detection
    pub content_hash: String,
    /// BLAKE3 hash of a *comment- and whitespace-normalized* form of the
    /// content, used as the embedding-reuse cache key (see
    /// `parser::chunk::canonical_hash`). Comment-only or formatting-only edits
    /// leave this stable so the embedding can be reused even though
    /// `content_hash` (the store identity) changed. Empty string means
    /// "not computed" (e.g. a Chunk hydrated from a DB row, where this field
    /// is never consulted — only freshly-parsed chunks at index time use it).
    pub canonical_hash: String,
    /// Parent chunk ID if this is a windowed portion of a larger chunk
    pub parent_id: Option<String>,
    /// Window index (0, 1, 2...) if this is a windowed portion
    pub window_idx: Option<u32>,
    /// Parent type name for methods (e.g., "CircuitBreaker" for `impl CircuitBreaker { ... }`)
    pub parent_type_name: Option<String>,
    /// Parser logic version stamp (see `parser::chunk::PARSER_VERSION`).
    /// Bumped when chunk-level extraction logic changes a non-content field
    /// (e.g., `doc` enrichment) so incremental UPSERT can refresh rows whose
    /// `content_hash` is unchanged.
    pub parser_version: u32,
}

/// Provenance of a call-graph edge — how the extractor decided this call
/// exists. Stored as a string in the `function_calls.edge_kind` column and
/// surfaced (skip-when-default) on callers/callees/impact/test-map entries so
/// a consuming agent can weight a syntactic `call_expression` differently from
/// a token-tree heuristic. Mirrors the `type_edges.edge_kind` precedent.
///
/// `Call` is the default and the overwhelming majority — it is the
/// skip-when-default value, so a present `edge_kind` always signals a
/// non-syntactic edge worth extra scrutiny.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Default, schemars::JsonSchema)]
// schemars-only snake_case so the SCHEMA matches the stored / wire strings
// (`serde_callback`, `macro_heuristic`, `fn_pointer`, `doc_reference`) that the
// `parse_edge_kind` deserializer accepts. Scoped to schemars (not serde) so the
// derived `Serialize` stays untouched — every output struct already routes the
// JSON through `serialize_edge_kind` / `as_str`, so this only shapes the input
// schema and leaves the (unused) derived `Serialize` byte-identical.
#[schemars(rename_all = "snake_case")]
pub enum CallEdgeKind {
    /// Syntactic `call_expression` — ground truth.
    #[default]
    Call,
    /// serde string-callback attribute (`#[serde(with = "...")]` etc.) — the
    /// attribute grammar is explicit, high confidence.
    SerdeCallback,
    /// `ident(`-shape match inside an opaque Rust macro token-tree —
    /// heuristic, no semantic resolution.
    MacroHeuristic,
    /// Bare function name passed as a fn-pointer / callback VALUE in argument
    /// position — heuristic, intra-file precision filter only.
    FnPointer,
    /// A Markdown cross-reference / link to a symbol — prose mention, not an
    /// invocation. Lowest evidentiary weight: a doc mention neither proves a
    /// function live (it stays a dead candidate) nor is it a heuristic caller
    /// (it cannot promote a function to `low-confidence-live`). Before this
    /// kind, markdown reference edges were mis-tagged `Call` and poisoned both
    /// the dead-code collapse rules and the call-graph collapse.
    DocReference,
}

/// Trust rank for a call-graph edge kind — lower is stronger evidence. Used by
/// the call-graph and dead-code MIN-collapse rules to pick the single most
/// authoritative kind among many edges to the same callee. Ordering is
/// EXPLICIT, never lexical: the old code collapsed by `MIN(edge_kind)` (string
/// alphabetical), which happened to rank `call` first only because `'c'` sorts
/// before `'f'/'m'/'s'` — a coincidence that [`CallEdgeKind::DocReference`]
/// ("doc_reference", `'d'`) breaks (it would sort second, ahead of the
/// heuristics, which is wrong). Single-sourced here and projected into SQL via
/// [`CallEdgeKind::rank_case_sql`].
///
/// Rank order, best (most trusted) to worst:
/// 0 `call` — syntactic call expression, ground truth.
/// 1 `serde_callback` — explicit attribute grammar, trusted.
/// 2 `macro_heuristic` — `ident(`-shape inside an opaque macro token-tree.
/// 3 `fn_pointer` — bare name in argument position, intra-file precision only.
/// 4 `doc_reference` — prose mention, weakest of all.
const fn call_edge_trust_rank(kind: CallEdgeKind) -> u8 {
    match kind {
        CallEdgeKind::Call => 0,
        CallEdgeKind::SerdeCallback => 1,
        CallEdgeKind::MacroHeuristic => 2,
        CallEdgeKind::FnPointer => 3,
        CallEdgeKind::DocReference => 4,
    }
}

impl CallEdgeKind {
    /// Every variant, in trust-rank order. Single source for SQL-list
    /// generators so a new kind cannot drift out of sync with the queries.
    pub const ALL: [CallEdgeKind; 5] = [
        CallEdgeKind::Call,
        CallEdgeKind::SerdeCallback,
        CallEdgeKind::MacroHeuristic,
        CallEdgeKind::FnPointer,
        CallEdgeKind::DocReference,
    ];

    /// String representation for database storage and JSON surfaces.
    pub fn as_str(&self) -> &'static str {
        match self {
            CallEdgeKind::Call => "call",
            CallEdgeKind::SerdeCallback => "serde_callback",
            CallEdgeKind::MacroHeuristic => "macro_heuristic",
            CallEdgeKind::FnPointer => "fn_pointer",
            CallEdgeKind::DocReference => "doc_reference",
        }
    }

    /// Parse from the stored string, defaulting to [`CallEdgeKind::Call`] for
    /// any unknown value — pre-v30 rows store the `'call'` default, and an
    /// unrecognized future kind degrades to the safe ground-truth label rather
    /// than failing the read.
    pub fn from_str_or_default(s: &str) -> Self {
        match s {
            "serde_callback" => CallEdgeKind::SerdeCallback,
            "macro_heuristic" => CallEdgeKind::MacroHeuristic,
            "fn_pointer" => CallEdgeKind::FnPointer,
            "doc_reference" => CallEdgeKind::DocReference,
            _ => CallEdgeKind::Call,
        }
    }

    /// Whether this edge is a heuristic (non-syntactic) caller. Used by the
    /// `dead` `low-confidence-live` verdict: a function whose only callers are
    /// heuristic edges may be a false-positive live. EXACTLY `macro_heuristic`
    /// and `fn_pointer` — serde callbacks are trusted and doc references are
    /// inert (prose, not a caller at all).
    pub fn is_heuristic(&self) -> bool {
        matches!(self, CallEdgeKind::MacroHeuristic | CallEdgeKind::FnPointer)
    }

    /// Whether this edge proves the callee genuinely live — a syntactic call or
    /// a trusted serde callback. A function with at least one trusted edge is
    /// never dead and never `low-confidence-live`. Heuristic and doc-reference
    /// edges do NOT count: doc references are prose, so a doc mention cannot
    /// disqualify a function from `low-confidence-live`.
    pub fn is_trusted(&self) -> bool {
        matches!(self, CallEdgeKind::Call | CallEdgeKind::SerdeCallback)
    }

    /// Whether this edge is an actual invocation of the callee — a real caller
    /// for dead-code purposes. Every kind EXCEPT `doc_reference` counts: a
    /// syntactic call, a serde callback, and the two heuristic kinds all reach
    /// the callee at runtime, while a `doc_reference` is a prose mention that
    /// invokes nothing. Defined as the complement of `doc_reference` (not an
    /// allow-list of the other four) so a future non-prose kind is a real caller
    /// by default — the only edge a function can survive `dead` candidacy on is a
    /// genuine invocation, never a doc link.
    pub fn is_real_caller(&self) -> bool {
        !matches!(self, CallEdgeKind::DocReference)
    }

    /// Explicit trust rank — lower is stronger evidence. See
    /// [`call_edge_trust_rank`] for the ordering rationale.
    pub fn trust_rank(&self) -> u8 {
        call_edge_trust_rank(*self)
    }

    /// Comma-separated quoted SQL string list of the `is_heuristic()` kinds,
    /// e.g. `'macro_heuristic', 'fn_pointer'`. Generated from the enum so the
    /// query kind-set is single-sourced — adding a heuristic variant updates
    /// every consumer automatically.
    pub fn heuristic_kinds_sql() -> String {
        Self::ALL
            .iter()
            .filter(|k| k.is_heuristic())
            .map(|k| format!("'{}'", k.as_str()))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Comma-separated quoted SQL string list of the `is_trusted()` kinds,
    /// e.g. `'call', 'serde_callback'`. Generated from the enum (single source).
    pub fn trusted_kinds_sql() -> String {
        Self::ALL
            .iter()
            .filter(|k| k.is_trusted())
            .map(|k| format!("'{}'", k.as_str()))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Comma-separated quoted SQL string list of the `is_real_caller()` kinds —
    /// every kind except `doc_reference`. The dead-candidate `NOT EXISTS`
    /// subquery uses this so a function whose only inbound edge is a prose
    /// `doc_reference` still qualifies as dead. Generated from the enum (single
    /// source), so it matches the `low-confidence-live` carve-out's view of what
    /// counts as a real caller.
    pub fn real_caller_kinds_sql() -> String {
        Self::ALL
            .iter()
            .filter(|k| k.is_real_caller())
            .map(|k| format!("'{}'", k.as_str()))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// A SQL `CASE` expression mapping `edge_kind` text to its integer
    /// [`trust_rank`](Self::trust_rank). Used in `MIN(...)`-collapse queries in
    /// place of the old lexical `MIN(edge_kind)`. Single-sourced from the enum
    /// so the rank order can never drift from [`call_edge_trust_rank`]. The
    /// `col` argument is the qualified column name (e.g. `fc.edge_kind`); it is
    /// caller-controlled SQL-identifier text, never user input.
    pub fn rank_case_sql(col: &str) -> String {
        let mut s = String::from("CASE ");
        for kind in Self::ALL {
            s.push_str(&format!(
                "WHEN {col} = '{}' THEN {} ",
                kind.as_str(),
                kind.trust_rank()
            ));
        }
        // Unknown / pre-v30 rows degrade to `call` (rank 0) — the same safe
        // default `from_str_or_default` uses on the read side.
        s.push_str(&format!("ELSE {} END", CallEdgeKind::Call.trust_rank()));
        s
    }
}

impl std::fmt::Display for CallEdgeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A function call site extracted from code
#[derive(Debug, Clone)]
pub struct CallSite {
    /// Name of the called function/method
    pub callee_name: String,
    /// Line number where the call occurs (1-indexed)
    pub line_number: u32,
    /// Provenance of this edge (syntactic vs heuristic). Defaults to
    /// [`CallEdgeKind::Call`] for the syntactic-call majority.
    pub kind: CallEdgeKind,
}

/// A function with its call sites (for full call graph coverage)
#[derive(Debug, Clone)]
pub struct FunctionCalls {
    /// Function name
    pub name: String,
    /// Starting line number (1-indexed)
    pub line_start: u32,
    /// Function calls made by this function
    pub calls: Vec<CallSite>,
}

/// A low-confidence call-graph candidate extracted from code.
///
/// Sibling of [`CallSite`], carried in a SEPARATE collection alongside
/// `Vec<CallSite>` so it can never be folded into the `function_calls` table.
/// Candidates land in the `candidate_edges` side-table (callee-name-keyed,
/// never joined by callers/callees/impact), so a candidate is invisible to the
/// caller graph by construction — it cannot become a false caller, and it
/// cannot move a callee into the wrong dead tier the way a new
/// [`CallEdgeKind`] variant would (that enum's `is_real_caller` is the
/// complement of `doc_reference`, so any new kind defaults to "real caller").
#[derive(Debug, Clone)]
pub struct CandidateSite {
    /// Source file the candidate reference lives in.
    pub file: PathBuf,
    /// Name of the symbol the candidate points at.
    pub callee_name: String,
    /// Line number of the reference (1-indexed).
    pub ref_line: u32,
    /// Candidate provenance string (the kind of low-confidence reference).
    pub candidate_kind: String,
}

/// Classification of how a type is referenced in code.
/// Used for type-level dependency tracking.
/// Stored as string in SQLite `type_edges.edge_kind` column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum TypeEdgeKind {
    /// Function/method parameter type: `fn foo(x: Config)`
    Param,
    /// Function/method return type: `fn foo() -> Config`
    Return,
    /// Struct/class field type: `struct Foo { config: Config }`
    Field,
    /// impl target, class extends/implements, interface embedding
    Impl,
    /// Trait/type parameter bound: `where T: Display`, `<T extends Foo>`
    Bound,
    /// Type alias target: `type Alias = Concrete`, typedef
    Alias,
}

impl TypeEdgeKind {
    /// String representation for database storage.
    pub fn as_str(&self) -> &'static str {
        match self {
            TypeEdgeKind::Param => "Param",
            TypeEdgeKind::Return => "Return",
            TypeEdgeKind::Field => "Field",
            TypeEdgeKind::Impl => "Impl",
            TypeEdgeKind::Bound => "Bound",
            TypeEdgeKind::Alias => "Alias",
        }
    }
}

impl std::fmt::Display for TypeEdgeKind {
    /// Formats the value using the given formatter by writing its string representation.
    /// # Arguments
    /// * `f` - The formatter to write the string representation to
    /// # Returns
    /// A `Result` indicating whether the formatting operation succeeded or failed
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for TypeEdgeKind {
    type Err = String;
    /// Parses a string into a TypeEdgeKind variant.
    /// # Arguments
    /// * `s` - A string slice representing a TypeEdgeKind. Valid values are "Param", "Return", "Field", "Impl", "Bound", and "Alias".
    /// # Returns
    /// Returns `Ok(TypeEdgeKind)` if the string matches a valid variant, or `Err(String)` with an error message if the string is unrecognized.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "Param" => Ok(TypeEdgeKind::Param),
            "Return" => Ok(TypeEdgeKind::Return),
            "Field" => Ok(TypeEdgeKind::Field),
            "Impl" => Ok(TypeEdgeKind::Impl),
            "Bound" => Ok(TypeEdgeKind::Bound),
            "Alias" => Ok(TypeEdgeKind::Alias),
            other => Err(format!("Unknown TypeEdgeKind: '{other}'")),
        }
    }
}

/// A type reference extracted from source code.
/// Captured by tree-sitter type queries with classified edge kinds.
/// The catch-all pattern captures types inside generics with `kind = None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeRef {
    /// Name of the referenced type (e.g., "Config", "Store", "SqlitePool")
    pub type_name: String,
    /// Line number where the reference occurs (1-indexed)
    pub line_number: u32,
    /// Edge classification, or None for types only found by catch-all (inside generics, etc.)
    pub kind: Option<TypeEdgeKind>,
}

/// A code element with its type references (for full-file type graph).
/// One entry per chunk (function/struct/enum/trait/class) in a file.
/// Produced by `Parser::parse_file_relationships()`.
#[derive(Debug, Clone)]
pub struct ChunkTypeRefs {
    /// Chunk name (function/struct/enum/trait/class)
    pub name: String,
    /// Starting line number (1-indexed)
    pub line_start: u32,
    /// Type references used by this chunk
    pub type_refs: Vec<TypeRef>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins the DERIVED `Serialize` spelling of `CallEdgeKind` (PascalCase
    /// variant names). Output today routes through `as_str` (snake_case) and the
    /// derived impl is unused, but the derive exists — so a future bare
    /// `#[serde(rename_all = ...)]` (e.g. someone "fixing" the schema casing in
    /// serde instead of schemars) would silently flip this the moment the derived
    /// impl reaches any output. Schema casing belongs to `#[schemars(rename_all)]`;
    /// serde output is a wire contract. This fails first if that line is crossed.
    #[test]
    fn call_edge_kind_derived_serialize_spelling_is_pinned() {
        assert_eq!(
            serde_json::to_string(&CallEdgeKind::Call).unwrap(),
            "\"Call\""
        );
        assert_eq!(
            serde_json::to_string(&CallEdgeKind::SerdeCallback).unwrap(),
            "\"SerdeCallback\""
        );
        assert_eq!(
            serde_json::to_string(&CallEdgeKind::MacroHeuristic).unwrap(),
            "\"MacroHeuristic\""
        );
        assert_eq!(
            serde_json::to_string(&CallEdgeKind::FnPointer).unwrap(),
            "\"FnPointer\""
        );
        assert_eq!(
            serde_json::to_string(&CallEdgeKind::DocReference).unwrap(),
            "\"DocReference\""
        );
    }

    /// Tests that all TypeEdgeKind variants can be converted to strings and parsed back to equal values.
    /// # Arguments
    /// None. This is a test function that operates on hardcoded TypeEdgeKind variants.
    /// # Returns
    /// None.
    /// # Panics
    /// Panics if parsing a stringified TypeEdgeKind fails or if a round-trip conversion does not produce an equal value.

    #[test]
    fn type_edge_kind_round_trip() {
        for kind in [
            TypeEdgeKind::Param,
            TypeEdgeKind::Return,
            TypeEdgeKind::Field,
            TypeEdgeKind::Impl,
            TypeEdgeKind::Bound,
            TypeEdgeKind::Alias,
        ] {
            let s = kind.to_string();
            let parsed: TypeEdgeKind = s.parse().unwrap();
            assert_eq!(kind, parsed, "Round-trip failed for {s}");
        }
    }

    /// `is_real_caller` is the complement of `doc_reference`: every other kind
    /// is a genuine invocation, only a doc reference is inert.
    #[test]
    fn is_real_caller_excludes_only_doc_reference() {
        assert!(CallEdgeKind::Call.is_real_caller());
        assert!(CallEdgeKind::SerdeCallback.is_real_caller());
        assert!(CallEdgeKind::MacroHeuristic.is_real_caller());
        assert!(CallEdgeKind::FnPointer.is_real_caller());
        assert!(!CallEdgeKind::DocReference.is_real_caller());
    }

    /// The real-caller SQL list carries every kind except `doc_reference`, and
    /// stays the union of the trusted + heuristic sets so the dead-candidate
    /// subquery agrees with the `low-confidence-live` carve-out on what counts.
    #[test]
    fn real_caller_kinds_sql_excludes_doc_reference() {
        let sql = CallEdgeKind::real_caller_kinds_sql();
        assert!(sql.contains("'call'"));
        assert!(sql.contains("'serde_callback'"));
        assert!(sql.contains("'macro_heuristic'"));
        assert!(sql.contains("'fn_pointer'"));
        assert!(
            !sql.contains("doc_reference"),
            "doc_reference must NOT appear in the real-caller kind set: {sql}"
        );
        // Real-caller kind count == ALL minus doc_reference.
        let real_count = CallEdgeKind::ALL
            .iter()
            .filter(|k| k.is_real_caller())
            .count();
        assert_eq!(real_count, CallEdgeKind::ALL.len() - 1);
    }
}
