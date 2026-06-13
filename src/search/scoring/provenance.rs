//! Per-result ranking provenance (`rank_signals`).
//!
//! Records *why* a search result ranked: which scoring signals contributed and
//! by how much. The vocabulary is the existing [`ScoreSignal`] / `SCORING_KNOBS`
//! names — no new taxonomy. Each entry's `value` is in the signal's native
//! unit: a 1-indexed rank for the retrieval legs (`dense`, `fts`, `sparse`), a
//! multiplier for the boost signals (`name_match`, `note_boost`, `type_boost`,
//! `parent_boost`).
//!
//! **Side channel, never a scoring change.** Recording reads the same inputs the
//! scoring fold consults but reproduces them in a separate pass that never feeds
//! back into the score. Scores and final order are bit-identical with recording
//! on or off; the `finalize_results` exact-equality tests pin this. The recorder
//! is invoked only when [`SearchFilter::record_rank_signals`](crate::store::helpers::SearchFilter)
//! is set.
//!
//! Discriminative-by-default: a signal is recorded only when it carries bits.
//! A boost is recorded only when it actually moved the score (multiplier ≠ 1.0,
//! or a positive name-match contribution), and only for results that went
//! through the dense scoring fold — an FTS-sole RRF hit never consumed the boost
//! multipliers, so they are not recorded for it. On the RRF path, where almost
//! every fused result appears in the keyword leg, `fts` is recorded only when
//! its leg rank materially leads the dense rank (or it is the sole leg);
//! recording it on every result would be cry-wolf. Note boosting is omitted
//! entirely under audit-mode suppression (it was forced to 1.0 in scoring). A
//! dense-only result with no boosts records a single `dense` rank — and a result
//! with no signals at all records nothing, so `rank_signals` stays
//! skip-when-empty on the wire.

use std::collections::HashMap;

use crate::language::ChunkType;
use crate::store::helpers::{RankSignal, SearchResult};

use super::candidate::chunk_importance;
use super::name_match::NameMatcher;
use super::note_boost::NoteBoost;

/// On the RRF path, record the `fts` leg only when its rank materially leads
/// the dense rank — i.e. the keyword leg placed the chunk at least this many
/// positions ahead of the semantic leg. On an explicit `--rrf` query nearly
/// every fused result appears in the FTS leg, so "in the keyword leg" is the
/// default, not a signal; requiring a material lead restores the
/// discriminative-by-default contract. A lead of 1 (strictly ahead) was too
/// permissive on dense-heavy corpora; a small margin keeps `fts` to the cases
/// where the literal-string match genuinely reordered the result.
const RRF_FTS_LEAD_MARGIN: usize = 3;

/// Read-only inputs the provenance pass consults to reconstruct per-result
/// signal contributions. Mirrors the pieces a [`super::candidate::ScoringContext`]
/// holds, plus the RRF leg rank lists and the type-boost set — everything the
/// scoring path already had in scope, threaded here so recording can run as a
/// separate pass without re-entering the score fold.
pub(crate) struct RankSignalCtx<'a> {
    /// Note-sentiment boost lookup (same instance the scoring fold used).
    pub note_index: &'a NoteBoost<'a>,
    /// Name matcher, present only when the query ran hybrid name-boost. `None`
    /// matches the scoring path's `NameBlend` gate (signal inactive).
    pub name_matcher: Option<&'a NameMatcher>,
    /// Whether test/underscore demotion was active (the `ImportanceDemotion`
    /// gate). When false the `importance` multiplier is never recorded.
    pub enable_demotion: bool,
    /// Chunk types boosted by adaptive routing, if any (the `type_boost` step).
    pub type_boost_types: Option<&'a [ChunkType]>,
    /// Multiplier applied by the type-boost step (resolved once by the caller
    /// from the same knob the score path used). Only consulted when a result's
    /// type is in `type_boost_types`.
    pub type_boost_factor: f32,
    /// 1-indexed dense rank per chunk id (the semantic/base ordering fed to
    /// fusion). Built once by the caller from the incoming `scored` list. A
    /// result with no entry here arrived only through the FTS leg (pure
    /// rank-fusion score) and never consumed the boost multipliers — see the
    /// `had_dense` gate in `signals_for`.
    pub dense_ranks: &'a HashMap<&'a str, usize>,
    /// 1-indexed FTS rank per chunk id, populated only on the RRF path.
    pub fts_ranks: &'a HashMap<&'a str, usize>,
    /// 1-indexed sparse (SPLADE) leg rank per chunk id, populated only on the
    /// hybrid SPLADE path. Mirrors the dense/FTS leg maps — the sparse leg is
    /// consumed inside `search_hybrid`, so its per-result rank is threaded out
    /// to the recording seam the same way.
    pub sparse_ranks: &'a HashMap<&'a str, usize>,
    /// Whether this search ran the RRF fusion path. On that path "appeared in
    /// the FTS leg" is the norm, so `fts` is recorded only when its leg rank
    /// materially leads the dense rank (or it's the sole leg). On the non-RRF
    /// path the FTS map is empty so the flag is inert.
    pub is_rrf: bool,
    /// Note-boost suppression (audit-mode): when set, the `note_boost` signal
    /// is never recorded — it never moved the score, so recording it would
    /// be dishonest. Mirrors the `NoteBoostSignal` suppression in scoring.
    pub suppress_note_boost: bool,
    /// Parent-boost multiplier per chunk id — populated for results the
    /// container hub-boost (`apply_parent_boost`) actually multiplied. Score-
    /// moving but applied in `finalize_results` after the scoring fold, so it's
    /// recorded here from the caller-computed map rather than reconstructed.
    pub parent_boosts: &'a HashMap<&'a str, f32>,
}

/// Caller-supplied half of the provenance inputs: the boost-lookup pieces that
/// live in the scoring caller (`search_filtered` / `search_by_candidate_ids`)
/// but not in `finalize_results`. The dense/FTS leg rank maps are built inside
/// `finalize_results` where the `scored` list and FTS leg are in scope, then
/// joined with these to form a [`RankSignalCtx`]. Present only when the search
/// opted into recording.
pub(crate) struct RankSignalInputs<'a> {
    /// Note-sentiment boost lookup (the same instance the scoring fold used).
    pub note_index: &'a NoteBoost<'a>,
    /// Name matcher, present only on the hybrid name-boost path.
    pub name_matcher: Option<&'a NameMatcher>,
    /// Whether test/underscore demotion was active.
    pub enable_demotion: bool,
    /// Note-boost suppression (audit-mode) — mirrors `filter.suppress_note_boost`.
    pub suppress_note_boost: bool,
    /// 1-indexed sparse (SPLADE) leg rank per chunk id. The sparse leg is
    /// consumed inside `search_hybrid` before `finalize_results`, so its
    /// per-result rank is captured there and threaded in (the dense/FTS legs are
    /// built inside `finalize_results`). Empty on the non-SPLADE paths.
    pub sparse_ranks: HashMap<String, usize>,
}

/// Derive the `rank_signals` array for one result. Pure function of the result
/// chunk's identity plus the shared [`RankSignalCtx`]; performs no store reads
/// and no score mutation.
pub(crate) fn signals_for(result: &SearchResult, ctx: &RankSignalCtx<'_>) -> Vec<RankSignal> {
    let name = result.chunk.name.as_str();
    let file = result.chunk.id.as_str();
    // The scoring path keys note/name/importance on the file portion of the
    // chunk id (`extract_file_from_chunk_id`); name matching keys on the chunk
    // name. Use the same `origin`-derived file the candidate scoring used.
    let file_part = super::filter::extract_file_from_chunk_id(file);

    // Whether the result went through the dense scoring fold. On the RRF path a
    // result with no dense rank arrived only via the FTS leg; its score is pure
    // rank fusion and never consumed the note/importance/name multipliers, so
    // recording those for it would be dishonest. On the non-RRF path
    // every result has a dense rank, so this gate is always satisfied there.
    let dense_rank = ctx.dense_ranks.get(result.chunk.id.as_str()).copied();
    let had_dense = dense_rank.is_some();

    // Discriminative signals first. The `dense` leg is the universal retrieval
    // path — every dense-scored result came through it, so a bare `dense` rank
    // carries no bits (the cry-wolf rule: a signal on every result is a
    // default). It is recorded only as *context* for a discriminative signal
    // below, never on its own.
    let mut discriminative: Vec<RankSignal> = Vec::new();

    // FTS leg. On the non-RRF path the FTS map is empty (inert). On the RRF
    // path nearly every fused result appears in the keyword leg, so "in FTS" is
    // the default, not a signal: record `fts` only when its rank
    // materially leads the dense rank, or when there's no dense rank at all
    // (the chunk is an FTS-sole hit, where the keyword leg is the whole story).
    if let Some(&fts_rank) = ctx.fts_ranks.get(result.chunk.id.as_str()) {
        let record_fts = if ctx.is_rrf {
            match dense_rank {
                // FTS materially ahead of dense → the keyword leg reordered it.
                Some(d) => fts_rank + RRF_FTS_LEAD_MARGIN <= d,
                // No dense rank → FTS is the sole leg that found it.
                None => true,
            }
        } else {
            // Non-RRF path keeps the prior "FTS is discriminative on its own"
            // semantics (the map is empty here in practice, so this is inert).
            true
        };
        if record_fts {
            discriminative.push(RankSignal {
                signal: "fts",
                value: fts_rank as f32,
            });
        }
    }

    // Sparse (SPLADE) leg. Recorded whenever the sparse leg contributed a rank
    // for this chunk — a retrieval leg like dense/FTS, surfaced so an
    // agent can distinguish "ranked by lexical sparse overlap" from "ranked by
    // dense semantics".
    if let Some(&sparse_rank) = ctx.sparse_ranks.get(result.chunk.id.as_str()) {
        discriminative.push(RankSignal {
            signal: "sparse",
            value: sparse_rank as f32,
        });
    }

    // Boost signals (name_match / note_boost / importance) only ran for
    // dense-scored results. Gate them on `had_dense` so FTS-sole RRF hits don't
    // report multipliers their pure-rank-fusion score never consumed.
    if had_dense {
        // name_match: the blended name-match score (native unit of the
        // NameBlend signal). Recorded only when the matcher is active and the
        // contribution is positive — a zero name score is the no-signal case.
        if let Some(matcher) = ctx.name_matcher {
            let name_score = matcher.score(name);
            if name_score > 0.0 {
                discriminative.push(RankSignal {
                    signal: "name_match",
                    value: name_score,
                });
            }
        }

        // note_boost: the sentiment multiplier. Recorded only when it actually
        // moved the score (≠ 1.0) and note boosting wasn't suppressed
        // (audit-mode) — under suppression the multiplier was forced to 1.0 in
        // scoring, so recording it would be dishonest.
        if !ctx.suppress_note_boost {
            let note_mult = ctx.note_index.boost(file_part, name);
            if note_mult != 1.0 {
                discriminative.push(RankSignal {
                    signal: "note_boost",
                    value: note_mult,
                });
            }
        }

        // importance: the test/underscore demotion multiplier (the
        // ImportanceDemotion signal). Only when demotion was active and
        // non-trivial.
        if ctx.enable_demotion {
            let imp = chunk_importance(name, file_part);
            if imp != 1.0 {
                discriminative.push(RankSignal {
                    signal: "importance",
                    value: imp,
                });
            }
        }
    }

    // parent_boost: the container hub-boost applied in `finalize_results` after
    // the scoring fold. Recorded from the caller-computed map for the
    // results it actually multiplied. Not gated on `had_dense` — it operates on
    // the final result set, which can include FTS-sole hits.
    if let Some(&boost) = ctx.parent_boosts.get(result.chunk.id.as_str()) {
        discriminative.push(RankSignal {
            signal: "parent_boost",
            value: boost,
        });
    }

    // type_boost: the adaptive-routing type multiplier (finalize step 4b).
    if let Some(types) = ctx.type_boost_types {
        if types.contains(&result.chunk.chunk_type) {
            discriminative.push(RankSignal {
                signal: "type_boost",
                value: ctx.type_boost_factor,
            });
        }
    }

    // No discriminative signal fired → record nothing (skip-when-empty on the
    // wire, zero token overhead). When one did, prepend the `dense` rank as
    // context so the consumer can read the boost relative to where the semantic
    // leg placed the chunk — but only when the result actually has a dense rank
    // (an FTS-sole RRF hit has no dense leg to report).
    if discriminative.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<RankSignal> = Vec::with_capacity(discriminative.len() + 1);
    if let Some(rank) = dense_rank {
        out.push(RankSignal {
            signal: "dense",
            value: rank as f32,
        });
    }
    out.extend(discriminative);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::helpers::{ChunkSummary, NoteSummary};
    use std::path::PathBuf;

    fn chunk(id: &str, name: &str, chunk_type: ChunkType) -> ChunkSummary {
        ChunkSummary {
            id: id.to_string(),
            file: PathBuf::from("src/lib.rs"),
            language: crate::parser::Language::Rust,
            chunk_type,
            name: name.to_string(),
            signature: String::new(),
            content: String::new(),
            doc: None,
            line_start: 1,
            line_end: 1,
            parent_id: None,
            parent_type_name: None,
            content_hash: String::new(),
            window_idx: None,
            parser_version: 0,
            vendored: false,
        }
    }

    fn note(sentiment: f32, mentions: &[&str]) -> NoteSummary {
        NoteSummary {
            id: "n".into(),
            text: "t".into(),
            sentiment,
            mentions: mentions.iter().map(|s| s.to_string()).collect(),
            kind: None,
        }
    }

    /// Empty leg / boost maps the simple-case tests share. Borrowing references
    /// to these keeps `RankSignalCtx` construction terse.
    type LegMap<'a> = HashMap<&'a str, usize>;
    type BoostMap<'a> = HashMap<&'a str, f32>;

    /// Build a `RankSignalCtx` with the common defaults. Caller passes the leg
    /// maps + parent-boost map by reference; everything else uses the
    /// non-discriminative defaults (no name matcher, demotion on, no type
    /// boost, non-RRF, notes live).
    #[allow(clippy::too_many_arguments)]
    fn ctx<'a>(
        note_index: &'a NoteBoost<'a>,
        dense: &'a LegMap<'a>,
        fts: &'a LegMap<'a>,
        sparse: &'a LegMap<'a>,
        parent_boosts: &'a BoostMap<'a>,
        type_boost_types: Option<&'a [ChunkType]>,
        is_rrf: bool,
        suppress_note_boost: bool,
    ) -> RankSignalCtx<'a> {
        RankSignalCtx {
            note_index,
            name_matcher: None,
            enable_demotion: true,
            type_boost_types,
            type_boost_factor: 1.2,
            dense_ranks: dense,
            fts_ranks: fts,
            sparse_ranks: sparse,
            is_rrf,
            suppress_note_boost,
            parent_boosts,
        }
    }

    #[test]
    fn dense_only_records_nothing() {
        // A pure dense-only hit (no FTS leg, no boosts) is the universal case;
        // recording a bare `dense` rank on it would carry zero bits. The
        // discriminative-default rule keeps `rank_signals` empty so it's
        // skip-when-empty on the wire.
        let note_index = NoteBoost::Borrowed(super::super::NoteBoostIndex::new(&[]));
        let mut dense = HashMap::new();
        dense.insert("src/lib.rs:1:foo", 3usize);
        let (fts, sparse, pb) = (HashMap::new(), HashMap::new(), HashMap::new());
        let c = ctx(&note_index, &dense, &fts, &sparse, &pb, None, false, false);
        let r = SearchResult::new(chunk("src/lib.rs:1:foo", "foo", ChunkType::Function), 0.9);
        assert!(signals_for(&r, &c).is_empty());
    }

    #[test]
    fn note_boost_recorded_with_dense_context() {
        // When a discriminative signal (note_boost) fires, the `dense` rank is
        // prepended as context so the consumer can read the boost relative to
        // the semantic placement.
        let notes = vec![note(1.0, &["foo"])];
        let note_index = NoteBoost::Borrowed(super::super::NoteBoostIndex::new(&notes));
        let mut dense = HashMap::new();
        dense.insert("src/lib.rs:1:foo", 2usize);
        let (fts, sparse, pb) = (HashMap::new(), HashMap::new(), HashMap::new());
        let c = ctx(&note_index, &dense, &fts, &sparse, &pb, None, false, false);
        let r = SearchResult::new(chunk("src/lib.rs:1:foo", "foo", ChunkType::Function), 0.9);
        let sigs = signals_for(&r, &c);
        assert_eq!(sigs[0].signal, "dense");
        assert_eq!(sigs[0].value, 2.0);
        assert!(sigs
            .iter()
            .any(|s| s.signal == "note_boost" && s.value > 1.0));
    }

    #[test]
    fn note_boost_omitted_when_suppressed() {
        // Audit-mode: suppression forces the multiplier to 1.0 in
        // scoring, so recording `note_boost` would be dishonest — it never
        // moved the score. The result records nothing.
        let notes = vec![note(1.0, &["foo"])];
        let note_index = NoteBoost::Borrowed(super::super::NoteBoostIndex::new(&notes));
        let mut dense = HashMap::new();
        dense.insert("src/lib.rs:1:foo", 2usize);
        let (fts, sparse, pb) = (HashMap::new(), HashMap::new(), HashMap::new());
        let c = ctx(
            &note_index,
            &dense,
            &fts,
            &sparse,
            &pb,
            None,
            false,
            true, // suppress_note_boost
        );
        let r = SearchResult::new(chunk("src/lib.rs:1:foo", "foo", ChunkType::Function), 0.9);
        let sigs = signals_for(&r, &c);
        assert!(
            !sigs.iter().any(|s| s.signal == "note_boost"),
            "suppressed note boost must not be recorded; got {sigs:?}"
        );
    }

    #[test]
    fn non_rrf_fts_records_when_present() {
        // On the non-RRF path the FTS map is empty in practice, but when a rank
        // is present it is recorded unconditionally (the prior semantics).
        let note_index = NoteBoost::Borrowed(super::super::NoteBoostIndex::new(&[]));
        let mut dense = HashMap::new();
        dense.insert("src/lib.rs:1:foo", 1usize);
        let mut fts = HashMap::new();
        fts.insert("src/lib.rs:1:foo", 4usize);
        let (sparse, pb) = (HashMap::new(), HashMap::new());
        let c = ctx(&note_index, &dense, &fts, &sparse, &pb, None, false, false);
        let r = SearchResult::new(chunk("src/lib.rs:1:foo", "foo", ChunkType::Function), 0.9);
        let sigs = signals_for(&r, &c);
        assert!(sigs.iter().any(|s| s.signal == "fts" && s.value == 4.0));
        assert!(sigs.iter().any(|s| s.signal == "dense" && s.value == 1.0));
    }

    #[test]
    fn rrf_fts_suppressed_when_not_leading() {
        // on the RRF path "appeared in the FTS leg" is the norm, not a
        // signal. A chunk whose FTS rank does not materially lead its dense rank
        // records no `fts` — and with no other signal, records nothing at all.
        let note_index = NoteBoost::Borrowed(super::super::NoteBoostIndex::new(&[]));
        let mut dense = HashMap::new();
        dense.insert("src/lib.rs:1:foo", 2usize);
        let mut fts = HashMap::new();
        fts.insert("src/lib.rs:1:foo", 3usize); // behind dense → not discriminative
        let (sparse, pb) = (HashMap::new(), HashMap::new());
        let c = ctx(&note_index, &dense, &fts, &sparse, &pb, None, true, false);
        let r = SearchResult::new(chunk("src/lib.rs:1:foo", "foo", ChunkType::Function), 0.9);
        assert!(
            signals_for(&r, &c).is_empty(),
            "FTS rank trailing dense must not record on the RRF path"
        );
    }

    #[test]
    fn rrf_fts_recorded_when_materially_leading() {
        // when the keyword leg places the chunk materially ahead of the
        // semantic leg, the literal-string match genuinely reordered it — record
        // `fts`.
        let note_index = NoteBoost::Borrowed(super::super::NoteBoostIndex::new(&[]));
        let mut dense = HashMap::new();
        dense.insert("src/lib.rs:1:foo", 10usize);
        let mut fts = HashMap::new();
        fts.insert("src/lib.rs:1:foo", 1usize); // 9 ahead → materially leads
        let (sparse, pb) = (HashMap::new(), HashMap::new());
        let c = ctx(&note_index, &dense, &fts, &sparse, &pb, None, true, false);
        let r = SearchResult::new(chunk("src/lib.rs:1:foo", "foo", ChunkType::Function), 0.9);
        let sigs = signals_for(&r, &c);
        assert!(sigs.iter().any(|s| s.signal == "fts" && s.value == 1.0));
    }

    #[test]
    fn rrf_fts_sole_leg_recorded() {
        // An FTS-sole RRF hit (no dense rank) records `fts` — the keyword leg is
        // the whole story — but no `dense` context (there's no dense leg).
        let note_index = NoteBoost::Borrowed(super::super::NoteBoostIndex::new(&[]));
        let dense = HashMap::new();
        let mut fts = HashMap::new();
        fts.insert("src/lib.rs:1:foo", 5usize);
        let (sparse, pb) = (HashMap::new(), HashMap::new());
        let c = ctx(&note_index, &dense, &fts, &sparse, &pb, None, true, false);
        let r = SearchResult::new(chunk("src/lib.rs:1:foo", "foo", ChunkType::Function), 0.9);
        let sigs = signals_for(&r, &c);
        assert!(sigs.iter().any(|s| s.signal == "fts" && s.value == 5.0));
        assert!(
            !sigs.iter().any(|s| s.signal == "dense"),
            "FTS-sole hit has no dense leg to report"
        );
    }

    #[test]
    fn fts_sole_hit_records_no_phantom_boosts() {
        // a result that arrived only through the FTS leg (no dense
        // rank) has a pure rank-fusion score that never consumed the
        // note/importance multipliers — they must not be recorded.
        let notes = vec![note(1.0, &["test_foo"])];
        let note_index = NoteBoost::Borrowed(super::super::NoteBoostIndex::new(&notes));
        let dense = HashMap::new(); // no dense rank → FTS-sole
        let mut fts = HashMap::new();
        fts.insert("src/lib.rs:1:test_foo", 2usize);
        let (sparse, pb) = (HashMap::new(), HashMap::new());
        let c = ctx(&note_index, &dense, &fts, &sparse, &pb, None, true, false);
        // test_foo would normally pick up both note_boost and importance.
        let r = SearchResult::new(
            chunk("src/lib.rs:1:test_foo", "test_foo", ChunkType::Function),
            0.9,
        );
        let sigs = signals_for(&r, &c);
        assert!(
            !sigs.iter().any(|s| s.signal == "note_boost"),
            "FTS-sole hit must not report note_boost; got {sigs:?}"
        );
        assert!(
            !sigs.iter().any(|s| s.signal == "importance"),
            "FTS-sole hit must not report importance; got {sigs:?}"
        );
        // The FTS leg itself (sole leg) still records.
        assert!(sigs.iter().any(|s| s.signal == "fts"));
    }

    #[test]
    fn sparse_leg_recorded() {
        // the SPLADE sparse leg is surfaced like dense/FTS.
        let note_index = NoteBoost::Borrowed(super::super::NoteBoostIndex::new(&[]));
        let mut dense = HashMap::new();
        dense.insert("src/lib.rs:1:foo", 2usize);
        let mut sparse = HashMap::new();
        sparse.insert("src/lib.rs:1:foo", 1usize);
        let (fts, pb) = (HashMap::new(), HashMap::new());
        let c = ctx(&note_index, &dense, &fts, &sparse, &pb, None, false, false);
        let r = SearchResult::new(chunk("src/lib.rs:1:foo", "foo", ChunkType::Function), 0.9);
        let sigs = signals_for(&r, &c);
        assert!(sigs.iter().any(|s| s.signal == "sparse" && s.value == 1.0));
        assert!(sigs.iter().any(|s| s.signal == "dense" && s.value == 2.0));
    }

    #[test]
    fn parent_boost_recorded() {
        // a result the container hub-boost multiplied records
        // `parent_boost` with its multiplier.
        let note_index = NoteBoost::Borrowed(super::super::NoteBoostIndex::new(&[]));
        let mut dense = HashMap::new();
        dense.insert("src/lib.rs:1:S", 2usize);
        let mut pb = HashMap::new();
        pb.insert("src/lib.rs:1:S", 1.1f32);
        let (fts, sparse) = (HashMap::new(), HashMap::new());
        let c = ctx(&note_index, &dense, &fts, &sparse, &pb, None, false, false);
        let r = SearchResult::new(chunk("src/lib.rs:1:S", "S", ChunkType::Struct), 0.9);
        let sigs = signals_for(&r, &c);
        assert!(sigs
            .iter()
            .any(|s| s.signal == "parent_boost" && (s.value - 1.1).abs() < 1e-6));
    }

    #[test]
    fn importance_recorded_only_when_demotion_enabled() {
        let note_index = NoteBoost::Borrowed(super::super::NoteBoostIndex::new(&[]));
        // Dense rank present so the boost-recording gate (`had_dense`) is met.
        let mut dense = HashMap::new();
        dense.insert("src/lib.rs:1:test_foo", 1usize);
        let (fts, sparse, pb) = (HashMap::new(), HashMap::new(), HashMap::new());
        let r = SearchResult::new(
            chunk("src/lib.rs:1:test_foo", "test_foo", ChunkType::Function),
            0.9,
        );

        let ctx_on = ctx(&note_index, &dense, &fts, &sparse, &pb, None, false, false);
        assert!(signals_for(&r, &ctx_on)
            .iter()
            .any(|s| s.signal == "importance"));

        let ctx_off = RankSignalCtx {
            enable_demotion: false,
            ..ctx_on
        };
        assert!(!signals_for(&r, &ctx_off)
            .iter()
            .any(|s| s.signal == "importance"));
    }

    #[test]
    fn type_boost_recorded_for_matching_type() {
        let note_index = NoteBoost::Borrowed(super::super::NoteBoostIndex::new(&[]));
        let mut dense = HashMap::new();
        dense.insert("src/lib.rs:1:S", 1usize);
        dense.insert("src/lib.rs:1:f", 2usize);
        let (fts, sparse, pb) = (HashMap::new(), HashMap::new(), HashMap::new());
        let boosted = [ChunkType::Struct];
        let c = ctx(
            &note_index,
            &dense,
            &fts,
            &sparse,
            &pb,
            Some(&boosted),
            false,
            false,
        );
        let matching = SearchResult::new(chunk("src/lib.rs:1:S", "S", ChunkType::Struct), 0.9);
        assert!(signals_for(&matching, &c)
            .iter()
            .any(|s| s.signal == "type_boost" && s.value == 1.2));

        let non_matching =
            SearchResult::new(chunk("src/lib.rs:1:f", "f", ChunkType::Function), 0.9);
        assert!(!signals_for(&non_matching, &c)
            .iter()
            .any(|s| s.signal == "type_boost"));
    }
}
