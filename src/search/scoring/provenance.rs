//! Per-result ranking provenance (`rank_signals`).
//!
//! Records *why* a search result ranked: which scoring signals contributed and
//! by how much. The vocabulary is the existing [`ScoreSignal`] / `SCORING_KNOBS`
//! names — no new taxonomy. Each entry's `value` is in the signal's native
//! unit: a 1-indexed rank for the RRF retrieval legs (`dense`, `fts`), a
//! multiplier for the boost signals (`name_match`, `note_boost`, `type_boost`).
//!
//! **Side channel, never a scoring change.** Recording reads the same inputs the
//! scoring fold consults but reproduces them in a separate pass that never feeds
//! back into the score. Scores and final order are bit-identical with recording
//! on or off; the `finalize_results` exact-equality tests pin this. The recorder
//! is invoked only when [`SearchFilter::record_rank_signals`](crate::store::helpers::SearchFilter)
//! is set.
//!
//! Discriminative-by-default: a boost is recorded only when it actually moved
//! the score (multiplier ≠ 1.0, or a positive name-match contribution). A
//! dense-only result with no boosts records a single `dense` rank — and a result
//! with no signals at all records nothing, so `rank_signals` stays
//! skip-when-empty on the wire.

use std::collections::HashMap;

use crate::language::ChunkType;
use crate::store::helpers::{RankSignal, SearchResult};

use super::candidate::chunk_importance;
use super::name_match::NameMatcher;
use super::note_boost::NoteBoost;

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
    /// fusion). Built once by the caller from the incoming `scored` list.
    pub dense_ranks: &'a HashMap<&'a str, usize>,
    /// 1-indexed FTS rank per chunk id, populated only on the RRF path.
    pub fts_ranks: &'a HashMap<&'a str, usize>,
}

/// Caller-supplied half of the provenance inputs: the boost-lookup pieces that
/// live in the scoring caller (`search_filtered` / `search_by_candidate_ids`)
/// but not in `finalize_results`. The leg rank maps are built inside
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

    // Discriminative signals first. The `dense` leg is the universal retrieval
    // path — every result came through it, so a bare `dense` rank carries no
    // bits (the cry-wolf rule: a signal on every result is a default). It is
    // therefore recorded only as *context* for a discriminative signal below,
    // never on its own. The FTS leg, by contrast, is discriminative: appearing
    // in the keyword leg distinguishes a literal-string match from a pure
    // concept match.
    let mut discriminative: Vec<RankSignal> = Vec::new();

    if let Some(&rank) = ctx.fts_ranks.get(result.chunk.id.as_str()) {
        discriminative.push(RankSignal {
            signal: "fts",
            value: rank as f32,
        });
    }

    // name_match: the blended name-match score (native unit of the NameBlend
    // signal). Recorded only when the matcher is active and the contribution is
    // positive — a zero name score is the no-signal case.
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
    // moved the score (≠ 1.0) — the audit-mode complement: a note that
    // influenced ranking is visible per-query without disabling notes.
    let note_mult = ctx.note_index.boost(file_part, name);
    if note_mult != 1.0 {
        discriminative.push(RankSignal {
            signal: "note_boost",
            value: note_mult,
        });
    }

    // importance: the test/underscore demotion multiplier (the
    // ImportanceDemotion signal). Only when demotion was active and non-trivial.
    if ctx.enable_demotion {
        let imp = chunk_importance(name, file_part);
        if imp != 1.0 {
            discriminative.push(RankSignal {
                signal: "importance",
                value: imp,
            });
        }
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

    // Pure dense-only results record nothing — skip-when-empty on the wire,
    // zero token overhead. When a discriminative signal fired, prepend the
    // `dense` rank as context so the consumer can read the boost relative to
    // where the semantic leg placed the chunk.
    if discriminative.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<RankSignal> = Vec::with_capacity(discriminative.len() + 1);
    if let Some(&rank) = ctx.dense_ranks.get(result.chunk.id.as_str()) {
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

    #[test]
    fn dense_only_records_nothing() {
        // A pure dense-only hit (no FTS leg, no boosts) is the universal case;
        // recording a bare `dense` rank on it would carry zero bits. The
        // discriminative-default rule keeps `rank_signals` empty so it's
        // skip-when-empty on the wire.
        let note_index = NoteBoost::Borrowed(super::super::NoteBoostIndex::new(&[]));
        let mut dense = HashMap::new();
        dense.insert("src/lib.rs:1:foo", 3usize);
        let fts = HashMap::new();
        let ctx = RankSignalCtx {
            note_index: &note_index,
            name_matcher: None,
            enable_demotion: true,
            type_boost_types: None,
            type_boost_factor: 1.2,
            dense_ranks: &dense,
            fts_ranks: &fts,
        };
        let r = SearchResult::new(chunk("src/lib.rs:1:foo", "foo", ChunkType::Function), 0.9);
        assert!(signals_for(&r, &ctx).is_empty());
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
        let fts = HashMap::new();
        let ctx = RankSignalCtx {
            note_index: &note_index,
            name_matcher: None,
            enable_demotion: true,
            type_boost_types: None,
            type_boost_factor: 1.2,
            dense_ranks: &dense,
            fts_ranks: &fts,
        };
        let r = SearchResult::new(chunk("src/lib.rs:1:foo", "foo", ChunkType::Function), 0.9);
        let sigs = signals_for(&r, &ctx);
        assert_eq!(sigs[0].signal, "dense");
        assert_eq!(sigs[0].value, 2.0);
        assert!(sigs
            .iter()
            .any(|s| s.signal == "note_boost" && s.value > 1.0));
    }

    #[test]
    fn fts_leg_is_discriminative_on_its_own() {
        // Appearing in the FTS keyword leg distinguishes a literal-string match
        // and is recorded even with no boosts.
        let note_index = NoteBoost::Borrowed(super::super::NoteBoostIndex::new(&[]));
        let mut dense = HashMap::new();
        dense.insert("src/lib.rs:1:foo", 1usize);
        let mut fts = HashMap::new();
        fts.insert("src/lib.rs:1:foo", 4usize);
        let ctx = RankSignalCtx {
            note_index: &note_index,
            name_matcher: None,
            enable_demotion: true,
            type_boost_types: None,
            type_boost_factor: 1.2,
            dense_ranks: &dense,
            fts_ranks: &fts,
        };
        let r = SearchResult::new(chunk("src/lib.rs:1:foo", "foo", ChunkType::Function), 0.9);
        let sigs = signals_for(&r, &ctx);
        assert!(sigs.iter().any(|s| s.signal == "fts" && s.value == 4.0));
        assert!(sigs.iter().any(|s| s.signal == "dense" && s.value == 1.0));
    }

    #[test]
    fn importance_recorded_only_when_demotion_enabled() {
        let note_index = NoteBoost::Borrowed(super::super::NoteBoostIndex::new(&[]));
        let dense = HashMap::new();
        let fts = HashMap::new();
        let r = SearchResult::new(
            chunk("src/lib.rs:1:test_foo", "test_foo", ChunkType::Function),
            0.9,
        );

        let ctx_on = RankSignalCtx {
            note_index: &note_index,
            name_matcher: None,
            enable_demotion: true,
            type_boost_types: None,
            type_boost_factor: 1.2,
            dense_ranks: &dense,
            fts_ranks: &fts,
        };
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
        let dense = HashMap::new();
        let fts = HashMap::new();
        let boosted = [ChunkType::Struct];
        let ctx = RankSignalCtx {
            note_index: &note_index,
            name_matcher: None,
            enable_demotion: true,
            type_boost_types: Some(&boosted),
            type_boost_factor: 1.2,
            dense_ranks: &dense,
            fts_ranks: &fts,
        };
        let matching = SearchResult::new(chunk("src/lib.rs:1:S", "S", ChunkType::Struct), 0.9);
        assert!(signals_for(&matching, &ctx)
            .iter()
            .any(|s| s.signal == "type_boost" && s.value == 1.2));

        let non_matching =
            SearchResult::new(chunk("src/lib.rs:1:f", "f", ChunkType::Function), 0.9);
        assert!(!signals_for(&non_matching, &ctx)
            .iter()
            .any(|s| s.signal == "type_boost"));
    }
}
