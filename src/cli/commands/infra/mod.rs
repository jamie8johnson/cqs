//! Infrastructure commands — init, doctor, audit mode, telemetry, projects, references, cache, ping, model

mod audit_mode;
mod cache_cmd;
#[cfg(feature = "convert")]
mod convert;
mod doctor;
mod init;
mod model;
mod ping;
mod project;
mod reference;
mod slot;
mod status;
mod telemetry_cmd;

pub(crate) use audit_mode::cmd_audit_mode;
pub(crate) use cache_cmd::{cmd_cache, CacheCommand};
#[cfg(feature = "convert")]
pub(crate) use convert::cmd_convert;
pub(crate) use doctor::cmd_doctor;
pub(crate) use init::cmd_init;
pub(crate) use model::{cmd_model, ModelCommand};
pub(crate) use ping::cmd_ping;
pub(crate) use project::{cmd_project, ProjectCommand};
pub(crate) use reference::{cmd_ref, RefCommand};
pub(crate) use slot::{cmd_slot, SlotCommand};
pub(crate) use status::cmd_status;
pub(crate) use telemetry_cmd::{cmd_telemetry, cmd_telemetry_reset};
