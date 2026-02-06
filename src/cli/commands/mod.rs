//! CLI command handlers
//!
//! Each submodule handles one CLI subcommand.

mod diff;
mod doctor;
mod explain;
mod graph;
mod index;
mod init;
mod notes;
mod query;
mod reference;
mod serve;
mod similar;
mod stats;

pub(crate) use diff::cmd_diff;
pub(crate) use doctor::cmd_doctor;
pub(crate) use explain::cmd_explain;
pub(crate) use graph::{cmd_callees, cmd_callers};
pub(crate) use index::cmd_index;
pub(crate) use init::cmd_init;
pub(crate) use notes::{cmd_notes, NotesCommand};
pub(crate) use query::cmd_query;
pub(crate) use reference::{cmd_ref, RefCommand};
pub(crate) use serve::{cmd_serve, ServeConfig};
pub(crate) use similar::cmd_similar;
pub(crate) use stats::cmd_stats;
