//! Audit mode tool - toggle note exclusion during audits

use anyhow::Result;
use chrono::Utc;
use serde_json::Value;

use super::super::server::McpServer;
use super::super::types::AuditModeArgs;
use super::super::validation::parse_duration;

/// Toggle or query audit mode
pub fn tool_audit_mode(server: &McpServer, arguments: Value) -> Result<Value> {
    let args: AuditModeArgs = serde_json::from_value(arguments)?;
    let mut audit_mode = server.audit_mode.lock().unwrap_or_else(|e| {
        tracing::debug!("Audit mode lock poisoned (prior panic), recovering");
        e.into_inner()
    });

    // If no enabled argument, just query current state
    if args.enabled.is_none() {
        let result = if audit_mode.is_active() {
            serde_json::json!({
                "audit_mode": true,
                "remaining": audit_mode.remaining(),
                "expires_at": audit_mode.expires_at.map(|t| t.to_rfc3339()),
            })
        } else {
            serde_json::json!({
                "audit_mode": false,
            })
        };

        return Ok(serde_json::json!({
            "content": [{
                "type": "text",
                "text": serde_json::to_string_pretty(&result)?
            }]
        }));
    }

    // SAFETY: early return above guarantees enabled is Some
    let Some(enabled) = args.enabled else {
        unreachable!("enabled checked above");
    };

    let cqs_dir = crate::resolve_index_dir(&server.project_root);

    if enabled {
        // Parse expires_in duration (default 30m)
        let expires_in = args.expires_in.as_deref().unwrap_or("30m");
        let duration = parse_duration(expires_in)?;
        let expires_at = Utc::now() + duration;

        audit_mode.enabled = true;
        audit_mode.expires_at = Some(expires_at);

        // Persist to disk so CLI can read the same state
        if let Err(e) = crate::audit::save_audit_state(&cqs_dir, &audit_mode) {
            tracing::warn!("Failed to persist audit mode: {}", e);
        }

        let result = serde_json::json!({
            "audit_mode": true,
            "message": "Audit mode enabled. Notes excluded from search and read.",
            "remaining": audit_mode.remaining(),
            "expires_at": expires_at.to_rfc3339(),
        });

        Ok(serde_json::json!({
            "content": [{
                "type": "text",
                "text": serde_json::to_string_pretty(&result)?
            }]
        }))
    } else {
        audit_mode.enabled = false;
        audit_mode.expires_at = None;

        // Persist to disk
        if let Err(e) = crate::audit::save_audit_state(&cqs_dir, &audit_mode) {
            tracing::warn!("Failed to persist audit mode: {}", e);
        }

        let result = serde_json::json!({
            "audit_mode": false,
            "message": "Audit mode disabled. Notes included in search and read.",
        });

        Ok(serde_json::json!({
            "content": [{
                "type": "text",
                "text": serde_json::to_string_pretty(&result)?
            }]
        }))
    }
}
