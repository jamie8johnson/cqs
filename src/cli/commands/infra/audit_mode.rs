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
// Args + core (surface-agnostic; audit-mode has no daemon path)
// ---------------------------------------------------------------------------

/// Input for [`audit_mode_core`]. `state == None` is the query path (report
/// current state without mutating); `Some(On/Off)` toggles and persists.
///
/// Owned + `Deserialize` like every other core Args, so a wire/MCP caller can
/// inflate it from `{"state": "on", "expires": "30m"}`. `expires` defaults to
/// `30m` when omitted, matching the clap default.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct AuditModeArgs {
    /// `None` → query current state; `Some(On)` / `Some(Off)` → set it.
    #[serde(default)]
    pub state: Option<AuditModeState>,
    /// Expiry duration for `On` (e.g. `30m`), parsed by `parse_duration`.
    /// Ignored on the query and `Off` paths.
    #[serde(default = "default_audit_expires")]
    pub expires: String,
}

/// `#[serde(default)]` for [`AuditModeArgs::expires`] — mirrors the clap
/// `--expires` default so wire callers that omit it get the same `30m`.
fn default_audit_expires() -> String {
    "30m".to_string()
}

/// Surface-agnostic core for `cqs audit-mode [on|off]`. Reads or persists the
/// audit-mode toggle under `cqs_dir` and returns the typed output the renderer
/// consumes. Mutating on the `On`/`Off` paths (writes `audit_state` via
/// `save_audit_state`); the query path is a pure read. No daemon path —
/// audit-mode is process-local posture set by the CLI before a review.
pub(crate) fn audit_mode_core(
    cqs_dir: &std::path::Path,
    args: &AuditModeArgs,
) -> Result<AuditModeOutput> {
    let _span = tracing::info_span!("audit_mode_core").entered();

    let Some(state) = args.state.as_ref() else {
        // Query path — report current state, no mutation.
        let mode = load_audit_state(cqs_dir);
        return Ok(if mode.is_active() {
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
        });
    };

    match state {
        AuditModeState::On => {
            let duration = parse_duration(&args.expires)?;
            let expires_at = Utc::now() + duration;
            let mode = AuditMode {
                enabled: true,
                expires_at: Some(expires_at),
            };
            save_audit_state(cqs_dir, &mode)?;
            Ok(AuditModeOutput {
                audit_mode: true,
                message: Some("Audit mode enabled. Notes excluded from search and read.".into()),
                remaining: mode.remaining(),
                expires_at: Some(expires_at.to_rfc3339()),
            })
        }
        AuditModeState::Off => {
            let mode = AuditMode {
                enabled: false,
                expires_at: None,
            };
            save_audit_state(cqs_dir, &mode)?;
            Ok(AuditModeOutput {
                audit_mode: false,
                message: Some("Audit mode disabled. Notes included in search and read.".into()),
                remaining: None,
                expires_at: None,
            })
        }
    }
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

    let output = audit_mode_core(
        &cqs_dir,
        &AuditModeArgs {
            state: state.cloned(),
            expires: expires.to_string(),
        },
    )?;

    if json {
        crate::cli::json_envelope::emit_json(&output)?;
        return Ok(());
    }

    // Text rendering reads the same typed output. The `expires` fallback in
    // the On branch reproduces the prior "Expires in <remaining-or-input>"
    // wording when `remaining()` can't format the duration.
    match (state, output.audit_mode) {
        (None, true) => println!(
            "Audit mode: ON ({})",
            output.remaining.as_deref().unwrap_or("no expiry")
        ),
        (None, false) => println!("Audit mode: OFF"),
        (Some(AuditModeState::On), _) => println!(
            "Audit mode enabled. Notes excluded. Expires in {}.",
            output.remaining.as_deref().unwrap_or(expires)
        ),
        (Some(AuditModeState::Off), _) => println!("Audit mode disabled. Notes included."),
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// `audit_mode_core` On→query→Off round-trips through the persisted state:
    /// On sets `audit_mode: true` with an expiry, a subsequent query reports
    /// it active, and Off clears it. Exercises the mutating + query paths
    /// without going through the CLI's `find_project_root`.
    #[test]
    fn audit_mode_core_on_query_off_roundtrip() {
        let dir = tempfile::TempDir::new().unwrap();
        let cqs_dir = dir.path();

        let on = audit_mode_core(
            cqs_dir,
            &AuditModeArgs {
                state: Some(AuditModeState::On),
                expires: "30m".to_string(),
            },
        )
        .unwrap();
        assert!(on.audit_mode);
        assert!(on.expires_at.is_some());

        let queried = audit_mode_core(
            cqs_dir,
            &AuditModeArgs {
                state: None,
                expires: "30m".to_string(),
            },
        )
        .unwrap();
        assert!(queried.audit_mode, "query after On must report active");

        let off = audit_mode_core(
            cqs_dir,
            &AuditModeArgs {
                state: Some(AuditModeState::Off),
                expires: "30m".to_string(),
            },
        )
        .unwrap();
        assert!(!off.audit_mode);

        let queried_off = audit_mode_core(
            cqs_dir,
            &AuditModeArgs {
                state: None,
                expires: "30m".to_string(),
            },
        )
        .unwrap();
        assert!(!queried_off.audit_mode, "query after Off must report off");
    }

    /// `AuditModeArgs` deserializes from the wire object every other core
    /// Args accepts. `state` accepts lowercase `"on"`/`"off"`; `expires`
    /// defaults to `30m` when omitted (matching the clap default).
    #[test]
    fn audit_mode_args_deserialize_from_wire() {
        let full: AuditModeArgs = serde_json::from_str(r#"{"state":"on","expires":"1h"}"#).unwrap();
        assert!(matches!(full.state, Some(AuditModeState::On)));
        assert_eq!(full.expires, "1h");

        let off: AuditModeArgs = serde_json::from_str(r#"{"state":"off"}"#).unwrap();
        assert!(matches!(off.state, Some(AuditModeState::Off)));
        assert_eq!(off.expires, "30m", "expires default mirrors clap --expires");

        let query: AuditModeArgs = serde_json::from_str("{}").unwrap();
        assert!(query.state.is_none(), "empty object → query path");
        assert_eq!(query.expires, "30m");
    }

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
