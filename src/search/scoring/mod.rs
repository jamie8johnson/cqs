//! Scoring algorithms, name matching, and search helpers.
//!
//! Split into submodules by concern:
//! - `config` - scoring configuration constants
//! - `knob` - shared resolver for f32 scoring knobs
//! - `name_match` - name matching/boosting logic
//! - `note_boost` - note-based score boosting
//! - `filter` - SQL filter building, glob compilation, chunk ID parsing
//! - `candidate` - candidate scoring, importance demotion, parent boost, bounded heap

mod candidate;
mod config;
mod filter;
mod fusion;
pub mod knob;
mod name_match;
mod note_boost;

pub(crate) use candidate::{
    apply_parent_boost, apply_scoring_pipeline, score_candidate, BoundedScoreHeap, ScoringContext,
};
#[cfg(test)]
pub(crate) use config::ScoringConfig;
pub(crate) use filter::{build_filter_sql, compile_glob_filter, extract_file_from_chunk_id};
pub(crate) use fusion::rrf_fuse;
pub use fusion::set_rrf_k_from_config;
pub(crate) use name_match::NameMatcher;
pub(crate) use note_boost::{NoteBoost, NoteBoostCache, NoteBoostIndex, OwnedNoteBoostIndex};
