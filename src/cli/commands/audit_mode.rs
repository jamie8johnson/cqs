//! Audit mode command for cqs
//!
//! Toggle audit mode to exclude notes from search/read results.
//! Useful for unbiased code review and fresh-eyes analysis.

use anyhow::{bail, Result};
use chrono::Utc;

use cqs::audit::{load_audit_state, save_audit_state, AuditMode};
use cqs::parse_duration;

use crate::cli::find_project_root;

/// Handle audit-mode command
pub(crate) fn cmd_audit_mode(state: Option<&str>, expires: &str, json: bool) -> Result<()> {
    let root = find_project_root();
    let cqs_dir = cqs::resolve_index_dir(&root);

    if !cqs_dir.exists() {
        bail!("No .cqs directory found. Run 'cqs init' first.");
    }

    // Query current state if no argument
    let Some(state) = state else {
        let mode = load_audit_state(&cqs_dir);
        if json {
            let result = if mode.is_active() {
                serde_json::json!({
                    "audit_mode": true,
                    "remaining": mode.remaining(),
                    "expires_at": mode.expires_at.map(|t| t.to_rfc3339()),
                })
            } else {
                serde_json::json!({
                    "audit_mode": false,
                })
            };
            println!("{}", serde_json::to_string_pretty(&result)?);
        } else if mode.is_active() {
            println!(
                "Audit mode: ON ({})",
                mode.remaining().unwrap_or_else(|| "no expiry".into())
            );
        } else {
            println!("Audit mode: OFF");
        }
        return Ok(());
    };

    match state {
        "on" => {
            let duration = parse_duration(expires)?;
            let expires_at = Utc::now() + duration;

            let mode = AuditMode {
                enabled: true,
                expires_at: Some(expires_at),
            };
            save_audit_state(&cqs_dir, &mode)?;

            if json {
                let result = serde_json::json!({
                    "audit_mode": true,
                    "message": "Audit mode enabled. Notes excluded from search and read.",
                    "remaining": mode.remaining(),
                    "expires_at": expires_at.to_rfc3339(),
                });
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                println!(
                    "Audit mode enabled. Notes excluded. Expires in {}.",
                    mode.remaining().unwrap_or_else(|| expires.to_string())
                );
            }
        }
        "off" => {
            let mode = AuditMode {
                enabled: false,
                expires_at: None,
            };
            save_audit_state(&cqs_dir, &mode)?;

            if json {
                let result = serde_json::json!({
                    "audit_mode": false,
                    "message": "Audit mode disabled. Notes included in search and read.",
                });
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                println!("Audit mode disabled. Notes included.");
            }
        }
        _ => {
            bail!("Invalid state '{}'. Use 'on' or 'off'.", state);
        }
    }

    Ok(())
}
