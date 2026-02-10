//! Input validation helpers for MCP server
//!
//! Security-critical validation functions for query length, duration parsing,
//! and path handling.

use anyhow::{bail, Result};

/// Maximum query length to prevent excessive embedding computation
pub const MAX_QUERY_LENGTH: usize = 8192;

/// Validate query: reject empty/whitespace-only and enforce max length.
pub fn validate_query_length(query: &str) -> Result<()> {
    if query.trim().is_empty() {
        bail!("Query is empty");
    }
    if query.len() > MAX_QUERY_LENGTH {
        bail!(
            "Query too long: {} bytes (max {})",
            query.len(),
            MAX_QUERY_LENGTH
        );
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
                bail!("Invalid duration '{}': missing number before 'h'", s);
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
                bail!("Invalid duration '{}': missing number before 'm'", s);
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
            bail!(
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
        bail!(
            "Invalid duration: '{}'. Use format like '30m', '1h', '2h30m'",
            s
        );
    }

    // Cap at 24 hours to prevent overflow and unreasonable values
    const MAX_MINUTES: i64 = 24 * 60;
    if total_minutes > MAX_MINUTES {
        bail!(
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
        // Bare number = minutes
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

    // ===== validate_query_length tests =====

    #[test]
    fn test_validate_query_empty_rejected() {
        assert!(validate_query_length("").is_err());
        assert!(validate_query_length("   ").is_err());
        assert!(validate_query_length("\t\n").is_err());
    }

    #[test]
    fn test_validate_query_normal_accepted() {
        assert!(validate_query_length("hello").is_ok());
        assert!(validate_query_length("  hello  ").is_ok());
    }

    #[test]
    fn test_parse_duration_missing_number() {
        assert!(parse_duration("m").is_err());
        assert!(parse_duration("h").is_err());
        assert!(parse_duration("hm").is_err());
    }

    #[test]
    fn test_validate_query_boundary_length() {
        // Exactly at limit should pass
        let at_limit = "a".repeat(MAX_QUERY_LENGTH);
        assert!(validate_query_length(&at_limit).is_ok());

        // One over limit should fail
        let over_limit = "a".repeat(MAX_QUERY_LENGTH + 1);
        assert!(validate_query_length(&over_limit).is_err());
    }

    #[test]
    fn test_parse_duration_overflow() {
        // i64::MAX hours should overflow
        assert!(parse_duration(&format!("{}h", i64::MAX)).is_err());
    }
}
