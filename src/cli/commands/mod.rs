//! CLI command handlers
//!
//! Each submodule handles one CLI subcommand.

mod doctor;
mod graph;
mod index;
mod init;
mod notes;
mod query;
mod reference;
mod serve;
mod stats;

pub(crate) use doctor::cmd_doctor;
pub(crate) use graph::{cmd_callees, cmd_callers};
pub(crate) use index::cmd_index;
pub(crate) use init::cmd_init;
pub(crate) use notes::{cmd_notes, NotesCommand};
pub(crate) use query::cmd_query;
pub(crate) use reference::{cmd_ref, RefCommand};
pub(crate) use serve::{cmd_serve, ServeConfig};
pub(crate) use stats::cmd_stats;
