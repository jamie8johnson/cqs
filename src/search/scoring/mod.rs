//! Scoring algorithms, name matching, and search helpers.
//!
//! Split into submodules by concern:
//! - `config` - scoring configuration constants
//! - `knob` - shared resolver for f32 scoring knobs (#1132)
//! - `name_match` - name matching/boosting logic
//! - `note_boost` - note-based score boosting
//! - `filter` - SQL filter building, glob compilation, chunk ID parsing
//! - `candidate` - candidate scoring, importance demotion, parent boost, bounded heap

mod candidate;
mod config;
pub(crate) mod knob;
mod filter;
mod name_match;
mod note_boost;

pub(crate) use candidate::{
    apply_parent_boost, apply_scoring_pipeline, score_candidate, BoundedScoreHeap, ScoringContext,
};
pub(crate) use filter::{build_filter_sql, compile_glob_filter, extract_file_from_chunk_id};
pub(crate) use name_match::NameMatcher;
pub(crate) use note_boost::{NoteBoost, NoteBoostIndex, OwnedNoteBoostIndex};
