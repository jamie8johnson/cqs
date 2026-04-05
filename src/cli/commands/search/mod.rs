//! Search commands — semantic code search, context assembly, exploration

pub(crate) mod gather;
mod neighbors;
mod onboard;
mod query;
mod related;
mod scout;
mod similar;
mod where_cmd;

pub(crate) use gather::{build_gather_output, cmd_gather, GatherContext};
pub(crate) use neighbors::cmd_neighbors;
pub(crate) use onboard::cmd_onboard;
pub(crate) use query::cmd_query;
pub(crate) use related::{build_related_output, cmd_related};
pub(crate) use scout::cmd_scout;
pub(crate) use similar::cmd_similar;
pub(crate) use where_cmd::{build_where_output, cmd_where};
