//! Infrastructure commands — init, doctor, audit mode, telemetry, projects, references

mod audit_mode;
#[cfg(feature = "convert")]
mod convert;
mod doctor;
mod init;
mod project;
mod reference;
mod telemetry_cmd;

pub(crate) use audit_mode::cmd_audit_mode;
#[cfg(feature = "convert")]
pub(crate) use convert::cmd_convert;
pub(crate) use doctor::cmd_doctor;
pub(crate) use init::cmd_init;
pub(crate) use project::{cmd_project, ProjectCommand};
pub(crate) use reference::{cmd_ref, RefCommand};
pub(crate) use telemetry_cmd::{cmd_telemetry, cmd_telemetry_reset};
