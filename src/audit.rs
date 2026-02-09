//! Audit mode for excluding notes from search/read
//!
//! During code audits or fresh-eyes reviews, audit mode prevents prior
//! observations from influencing analysis by excluding notes from results.
//!
//! State is persisted to `.cqs/audit-mode.json` so both CLI and MCP can
//! share audit mode state across invocations.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Audit mode state - excludes notes from search/read during audits
#[derive(Default)]
pub struct AuditMode {
    pub enabled: bool,
    pub expires_at: Option<DateTime<Utc>>,
}

impl AuditMode {
    /// Check if audit mode is currently active (enabled and not expired)
    pub fn is_active(&self) -> bool {
        if !self.enabled {
            return false;
        }
        match self.expires_at {
            Some(expires) => Utc::now() < expires,
            None => true,
        }
    }

    /// Get remaining time as human-readable string, or None if expired/disabled
    pub fn remaining(&self) -> Option<String> {
        if !self.is_active() {
            return None;
        }
        let expires = self.expires_at?;
        let remaining = expires - Utc::now();
        let minutes = remaining.num_minutes();
        if minutes <= 0 {
            None
        } else if minutes < 60 {
            Some(format!("{}m", minutes))
        } else {
            Some(format!("{}h {}m", minutes / 60, minutes % 60))
        }
    }

    /// Format status line for inclusion in responses
    pub fn status_line(&self) -> Option<String> {
        let remaining = self.remaining()?;
        Some(format!(
            "(audit mode: notes excluded, {} remaining)",
            remaining
        ))
    }
}

/// Persisted audit mode state
#[derive(Serialize, Deserialize)]
struct AuditModeFile {
    enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    expires_at: Option<String>,
}

/// Load audit mode state from `.cqs/audit-mode.json`.
/// Returns default (inactive) if file is missing, expired, or unreadable.
pub fn load_audit_state(cqs_dir: &Path) -> AuditMode {
    let path = cqs_dir.join("audit-mode.json");
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return AuditMode::default(),
    };
    let file: AuditModeFile = match serde_json::from_str(&content) {
        Ok(f) => f,
        Err(e) => {
            tracing::debug!("Failed to parse audit-mode.json: {}", e);
            return AuditMode::default();
        }
    };

    let expires_at = file.expires_at.and_then(|s| {
        DateTime::parse_from_rfc3339(&s)
            .map(|dt| dt.with_timezone(&Utc))
            .map_err(|e| tracing::debug!("Failed to parse expires_at: {}", e))
            .ok()
    });

    let mode = AuditMode {
        enabled: file.enabled,
        expires_at,
    };

    // If expired, treat as inactive (but don't delete the file — harmless)
    if file.enabled && !mode.is_active() {
        return AuditMode::default();
    }

    mode
}

/// Save audit mode state to `.cqs/audit-mode.json`.
pub fn save_audit_state(cqs_dir: &Path, mode: &AuditMode) -> Result<()> {
    let path = cqs_dir.join("audit-mode.json");
    let file = AuditModeFile {
        enabled: mode.enabled,
        expires_at: mode.expires_at.map(|t| t.to_rfc3339()),
    };
    let content = serde_json::to_string_pretty(&file).context("Failed to serialize audit mode")?;
    std::fs::write(&path, content).context("Failed to write audit-mode.json")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audit_mode_default_inactive() {
        let mode = AuditMode::default();
        assert!(!mode.is_active());
    }

    #[test]
    fn test_audit_mode_enabled_active() {
        let mode = AuditMode {
            enabled: true,
            expires_at: None,
        };
        assert!(mode.is_active());
    }

    #[test]
    fn test_audit_mode_expired_inactive() {
        let mode = AuditMode {
            enabled: true,
            expires_at: Some(Utc::now() - chrono::Duration::hours(1)),
        };
        assert!(!mode.is_active());
    }

    #[test]
    fn test_audit_mode_not_expired_active() {
        let mode = AuditMode {
            enabled: true,
            expires_at: Some(Utc::now() + chrono::Duration::hours(1)),
        };
        assert!(mode.is_active());
    }

    #[test]
    fn test_save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let mode = AuditMode {
            enabled: true,
            expires_at: Some(Utc::now() + chrono::Duration::hours(1)),
        };
        save_audit_state(dir.path(), &mode).unwrap();
        let loaded = load_audit_state(dir.path());
        assert!(loaded.is_active());
        assert!(loaded.enabled);
    }

    #[test]
    fn test_load_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let loaded = load_audit_state(dir.path());
        assert!(!loaded.is_active());
    }

    #[test]
    fn test_load_expired_returns_inactive() {
        let dir = tempfile::tempdir().unwrap();
        let mode = AuditMode {
            enabled: true,
            expires_at: Some(Utc::now() - chrono::Duration::hours(1)),
        };
        save_audit_state(dir.path(), &mode).unwrap();
        let loaded = load_audit_state(dir.path());
        assert!(!loaded.is_active());
    }

    #[test]
    fn test_save_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let mode = AuditMode {
            enabled: false,
            expires_at: None,
        };
        save_audit_state(dir.path(), &mode).unwrap();
        let loaded = load_audit_state(dir.path());
        assert!(!loaded.is_active());
    }
}
