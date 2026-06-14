//! Review commands — diff review, CI analysis, dead code, health checks

mod affected;
pub(crate) mod ci;
pub(crate) mod dead;
pub(crate) mod diff_review;
pub(crate) mod health;
pub(crate) mod suggest;

pub(crate) use affected::cmd_affected;
pub(crate) use ci::{ci_core, cmd_ci, CiArgs};
pub(crate) use dead::{cmd_dead, dead_overlay, DeadArgs, DeadVerdict};
// `dead_core` (no-overlay entry point) is consumed only by the test-gated
// re-export in `commands/mod.rs`; production routes through `dead_overlay`.
#[cfg(test)]
pub(crate) use dead::dead_core;
pub(crate) use diff_review::{cmd_review, review_core, ReviewArgs};
pub(crate) use health::{cmd_health, health_core, HealthArgs};
pub(crate) use suggest::{cmd_suggest, suggest_core, SuggestArgs};
