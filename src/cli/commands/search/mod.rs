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
pub(crate) use related::{cmd_related, related_result_to_json};
pub(crate) use scout::cmd_scout;
pub(crate) use similar::cmd_similar;
pub(crate) use where_cmd::{cmd_where, where_to_json};
