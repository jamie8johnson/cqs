//! Audit mode for excluding notes from search/read
//!
//! During code audits or fresh-eyes reviews, audit mode prevents prior
//! observations from influencing analysis by excluding notes from results.

use chrono::{DateTime, Utc};

/// Audit mode state - excludes notes from search/read during audits
#[derive(Default)]
pub(crate) struct AuditMode {
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
}
