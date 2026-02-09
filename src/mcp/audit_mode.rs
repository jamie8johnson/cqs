//! Audit mode re-export for MCP module
//!
//! The canonical implementation lives in `crate::audit`. This module
//! re-exports it for backward compatibility within the MCP server.

pub(crate) use crate::audit::AuditMode;
