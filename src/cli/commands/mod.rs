//! CLI command handlers
//!
//! Each submodule handles one CLI subcommand.

mod audit_mode;
mod context;
mod dead;
mod diff;
mod doctor;
mod explain;
mod gather;
mod gc;
mod graph;
mod impact;
mod impact_diff;
mod index;
mod init;
mod notes;
mod project;
mod query;
mod read;
mod reference;
mod related;
pub(crate) mod resolve;
mod similar;
mod stale;
mod stats;
mod test_map;
mod trace;

pub(crate) use audit_mode::cmd_audit_mode;
pub(crate) use context::cmd_context;
pub(crate) use dead::cmd_dead;
pub(crate) use diff::cmd_diff;
pub(crate) use doctor::cmd_doctor;
pub(crate) use explain::cmd_explain;
pub(crate) use gather::cmd_gather;
pub(crate) use gc::cmd_gc;
pub(crate) use graph::{cmd_callees, cmd_callers};
pub(crate) use impact::cmd_impact;
pub(crate) use impact_diff::cmd_impact_diff;
pub(crate) use index::{build_hnsw_index, cmd_index};
pub(crate) use init::cmd_init;
pub(crate) use notes::{cmd_notes, NotesCommand};
pub(crate) use project::{cmd_project, ProjectCommand};
pub(crate) use query::cmd_query;
pub(crate) use read::cmd_read;
pub(crate) use reference::{cmd_ref, RefCommand};
pub(crate) use related::cmd_related;
pub(crate) use similar::cmd_similar;
pub(crate) use stale::cmd_stale;
pub(crate) use stats::cmd_stats;
pub(crate) use test_map::cmd_test_map;
pub(crate) use trace::cmd_trace;
