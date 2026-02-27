//! Audit mode for excluding notes from search/read
//!
//! During code audits or fresh-eyes reviews, audit mode prevents prior
//! observations from influencing analysis by excluding notes from results.
//!
//! State is persisted to `.cqs/audit-mode.json` so audit mode state
//! survives across CLI invocations.

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

    // Atomic write: temp file + rename
    let tmp_path = path.with_extension(format!(
        "json.tmp.{}.{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    std::fs::write(&tmp_path, &content).context("Failed to write temp audit-mode file")?;
    if let Err(rename_err) = std::fs::rename(&tmp_path, &path) {
        if let Err(copy_err) = std::fs::copy(&tmp_path, &path) {
            let _ = std::fs::remove_file(&tmp_path);
            anyhow::bail!(
                "rename failed ({}), copy fallback failed: {}",
                rename_err,
                copy_err
            );
        }
        let _ = std::fs::remove_file(&tmp_path);
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// Parse duration string like "30m", "1h", "2h30m" into chrono::Duration
pub fn parse_duration(s: &str) -> Result<chrono::Duration> {
    let s = s.trim().to_lowercase();
    let mut total_minutes: i64 = 0;
    let mut current_num = String::new();

    for c in s.chars() {
        if c.is_ascii_digit() {
            current_num.push(c);
        } else if c == 'h' {
            if current_num.is_empty() {
                anyhow::bail!("Invalid duration '{}': missing number before 'h'", s);
            }
            let hours: i64 = current_num.parse().map_err(|_| {
                anyhow::anyhow!(
                    "Invalid duration '{}': '{}' is not a valid number",
                    s,
                    current_num
                )
            })?;
            total_minutes = hours
                .checked_mul(60)
                .and_then(|m| total_minutes.checked_add(m))
                .ok_or_else(|| anyhow::anyhow!("Duration overflow in '{}'", s))?;
            current_num.clear();
        } else if c == 'm' {
            if current_num.is_empty() {
                anyhow::bail!("Invalid duration '{}': missing number before 'm'", s);
            }
            let mins: i64 = current_num.parse().map_err(|_| {
                anyhow::anyhow!(
                    "Invalid duration '{}': '{}' is not a valid number",
                    s,
                    current_num
                )
            })?;
            total_minutes = total_minutes
                .checked_add(mins)
                .ok_or_else(|| anyhow::anyhow!("Duration overflow in '{}'", s))?;
            current_num.clear();
        } else if !c.is_whitespace() {
            anyhow::bail!(
                "Invalid duration '{}': unexpected character '{}'. Use format like '30m', '1h', '2h30m'",
                s, c
            );
        }
    }

    // Handle bare number (assume minutes)
    if !current_num.is_empty() {
        let mins: i64 = current_num.parse().map_err(|_| {
            anyhow::anyhow!(
                "Invalid duration '{}': '{}' is not a valid number",
                s,
                current_num
            )
        })?;
        total_minutes = total_minutes
            .checked_add(mins)
            .ok_or_else(|| anyhow::anyhow!("Duration overflow in '{}'", s))?;
    }

    if total_minutes <= 0 {
        anyhow::bail!(
            "Invalid duration: '{}'. Use format like '30m', '1h', '2h30m'",
            s
        );
    }

    // Cap at 24 hours to prevent overflow and unreasonable values
    const MAX_MINUTES: i64 = 24 * 60;
    if total_minutes > MAX_MINUTES {
        anyhow::bail!(
            "Duration too long: {} minutes (max {} minutes / 24 hours)",
            total_minutes,
            MAX_MINUTES
        );
    }

    Ok(chrono::Duration::minutes(total_minutes))
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

    // ===== parse_duration tests =====

    #[test]
    fn test_parse_duration_minutes() {
        assert_eq!(
            parse_duration("30m").unwrap(),
            chrono::Duration::minutes(30)
        );
        assert_eq!(parse_duration("1m").unwrap(), chrono::Duration::minutes(1));
        assert_eq!(
            parse_duration("120m").unwrap(),
            chrono::Duration::minutes(120)
        );
    }

    #[test]
    fn test_parse_duration_hours() {
        assert_eq!(parse_duration("1h").unwrap(), chrono::Duration::minutes(60));
        assert_eq!(
            parse_duration("2h").unwrap(),
            chrono::Duration::minutes(120)
        );
    }

    #[test]
    fn test_parse_duration_combined() {
        assert_eq!(
            parse_duration("1h30m").unwrap(),
            chrono::Duration::minutes(90)
        );
        assert_eq!(
            parse_duration("2h15m").unwrap(),
            chrono::Duration::minutes(135)
        );
    }

    #[test]
    fn test_parse_duration_bare_number() {
        assert_eq!(parse_duration("30").unwrap(), chrono::Duration::minutes(30));
    }

    #[test]
    fn test_parse_duration_whitespace() {
        assert_eq!(
            parse_duration("  30m  ").unwrap(),
            chrono::Duration::minutes(30)
        );
        assert_eq!(
            parse_duration("1h 30m").unwrap(),
            chrono::Duration::minutes(90)
        );
    }

    #[test]
    fn test_parse_duration_case_insensitive() {
        assert_eq!(
            parse_duration("30M").unwrap(),
            chrono::Duration::minutes(30)
        );
        assert_eq!(parse_duration("1H").unwrap(), chrono::Duration::minutes(60));
    }

    #[test]
    fn test_parse_duration_invalid_character() {
        assert!(parse_duration("30x").is_err());
        assert!(parse_duration("abc").is_err());
    }

    #[test]
    fn test_parse_duration_zero() {
        assert!(parse_duration("0m").is_err());
        assert!(parse_duration("0").is_err());
    }

    #[test]
    fn test_parse_duration_empty() {
        assert!(parse_duration("").is_err());
        assert!(parse_duration("   ").is_err());
    }

    #[test]
    fn test_parse_duration_missing_number() {
        assert!(parse_duration("m").is_err());
        assert!(parse_duration("h").is_err());
        assert!(parse_duration("hm").is_err());
    }

    #[test]
    fn test_parse_duration_overflow() {
        assert!(parse_duration(&format!("{}h", i64::MAX)).is_err());
    }
}
