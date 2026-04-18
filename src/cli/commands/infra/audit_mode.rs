//! Audit mode command for cqs
//!
//! Toggle audit mode to exclude notes from search/read results.
//! Useful for unbiased code review and fresh-eyes analysis.
//!
//! Core struct is [`AuditModeOutput`]; built inline in the command handler.

use anyhow::{bail, Result};
use chrono::Utc;

use cqs::audit::{load_audit_state, save_audit_state, AuditMode};
use cqs::parse_duration;

use crate::cli::find_project_root;
use crate::cli::AuditModeState;

// ---------------------------------------------------------------------------
// Output struct
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Serialize)]
pub(crate) struct AuditModeOutput {
    pub audit_mode: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remaining: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

// ---------------------------------------------------------------------------
// CLI command
// ---------------------------------------------------------------------------

pub(crate) fn cmd_audit_mode(
    state: Option<&AuditModeState>,
    expires: &str,
    json: bool,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_audit_mode").entered();
    let root = find_project_root();
    let cqs_dir = cqs::resolve_index_dir(&root);

    if !cqs_dir.exists() {
        bail!("No .cqs directory found. Run 'cqs init' first.");
    }

    // Query current state if no argument
    let Some(state) = state else {
        let mode = load_audit_state(&cqs_dir);
        if json {
            let output = if mode.is_active() {
                AuditModeOutput {
                    audit_mode: true,
                    message: None,
                    remaining: mode.remaining(),
                    expires_at: mode.expires_at.map(|t| t.to_rfc3339()),
                }
            } else {
                AuditModeOutput {
                    audit_mode: false,
                    message: None,
                    remaining: None,
                    expires_at: None,
                }
            };
            crate::cli::json_envelope::emit_json(&output)?;
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
        AuditModeState::On => {
            let duration = parse_duration(expires)?;
            let expires_at = Utc::now() + duration;

            let mode = AuditMode {
                enabled: true,
                expires_at: Some(expires_at),
            };
            save_audit_state(&cqs_dir, &mode)?;

            if json {
                let output = AuditModeOutput {
                    audit_mode: true,
                    message: Some(
                        "Audit mode enabled. Notes excluded from search and read.".into(),
                    ),
                    remaining: mode.remaining(),
                    expires_at: Some(expires_at.to_rfc3339()),
                };
                crate::cli::json_envelope::emit_json(&output)?;
            } else {
                println!(
                    "Audit mode enabled. Notes excluded. Expires in {}.",
                    mode.remaining().unwrap_or_else(|| expires.to_string())
                );
            }
        }
        AuditModeState::Off => {
            let mode = AuditMode {
                enabled: false,
                expires_at: None,
            };
            save_audit_state(&cqs_dir, &mode)?;

            if json {
                let output = AuditModeOutput {
                    audit_mode: false,
                    message: Some("Audit mode disabled. Notes included in search and read.".into()),
                    remaining: None,
                    expires_at: None,
                };
                crate::cli::json_envelope::emit_json(&output)?;
            } else {
                println!("Audit mode disabled. Notes included.");
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audit_mode_output_active() {
        let output = AuditModeOutput {
            audit_mode: true,
            message: Some("Audit mode enabled. Notes excluded from search and read.".into()),
            remaining: Some("29m".into()),
            expires_at: Some("2026-04-02T12:00:00+00:00".into()),
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["audit_mode"], true);
        assert_eq!(
            json["message"],
            "Audit mode enabled. Notes excluded from search and read."
        );
        assert_eq!(json["remaining"], "29m");
        assert!(json["expires_at"].as_str().unwrap().contains("2026"));
    }

    #[test]
    fn test_audit_mode_output_inactive() {
        let output = AuditModeOutput {
            audit_mode: false,
            message: None,
            remaining: None,
            expires_at: None,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["audit_mode"], false);
        // Optional fields omitted
        assert!(json.get("message").is_none());
        assert!(json.get("remaining").is_none());
        assert!(json.get("expires_at").is_none());
    }
}
