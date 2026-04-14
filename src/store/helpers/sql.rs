//! SQL placeholder generation, caching, and batch-size derivation.
//!
//! ## SQLite variable limit
//!
//! `SQLITE_MAX_VARIABLE_NUMBER` was 999 before v3.32 (2020) and is 32766
//! in current SQLite. cqs requires SQLite 3.35+ (for RETURNING) so the
//! limit is always 32766. The v1.22.0 audit (SHL-31/32/33) found 15 call
//! sites still using the old 999-derived batch sizes, producing 10-30×
//! more SQL statements than necessary. The [`max_rows_per_statement`]
//! helper centralizes the derivation so call sites don't need to
//! re-derive the constant.

/// Read `CQS_BUSY_TIMEOUT_MS` env var, falling back to `default_ms`. Single
/// source of truth so every SQLite pool (store, embedding cache, query
/// cache) honours the same tuning knob.
pub fn busy_timeout_from_env(default_ms: u64) -> std::time::Duration {
    let ms = std::env::var("CQS_BUSY_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(default_ms);
    std::time::Duration::from_millis(ms)
}

/// SQLite's `SQLITE_MAX_VARIABLE_NUMBER` since v3.32 (2020).
/// Single source of truth — all batch-size derivations reference this.
pub const SQLITE_MAX_VARIABLES: usize = 32766;

/// Generic headroom so a future caller adding one more bind variable
/// doesn't instantly trip the limit. NOT sized to absorb a full extra
/// column; adding a new column requires updating `vars_per_row` at the
/// call site (SHL-41 audit rationale correction).
pub const SAFETY_MARGIN_VARS: usize = 300;

/// Derive the maximum rows per INSERT/DELETE statement given the number
/// of bind variables per row. Centralizes the `(LIMIT - MARGIN) / N`
/// derivation that was previously inlined (and wrong) at 15+ sites.
///
/// For single-bind queries (e.g. `WHERE id IN (?, ?, ...)`), pass
/// `vars_per_row = 1`. For multi-column INSERTs, pass the column count.
pub const fn max_rows_per_statement(vars_per_row: usize) -> usize {
    (SQLITE_MAX_VARIABLES - SAFETY_MARGIN_VARS) / vars_per_row
}

/// Maximum batch size that is pre-built and cached at startup.
///
/// SHL-V1.25-14: sized exactly to cover the caller-facing max
/// (`max_rows_per_statement(1) = SQLITE_MAX_VARIABLES - SAFETY_MARGIN_VARS
/// = 32466`). With the previous 10_000 cap, single-bind batches beyond
/// 10k fell off the cache and re-built the ~120KB placeholder string
/// every call, negating the cache's purpose. The extra ~22k strings
/// cost ~1-2MB at startup in exchange for zero-alloc on the hot path.
const PLACEHOLDER_CACHE_MAX: usize = SQLITE_MAX_VARIABLES - SAFETY_MARGIN_VARS;

/// Pre-built placeholder strings for n = 1..=PLACEHOLDER_CACHE_MAX.
/// Index 0 is unused; index n holds the string for n placeholders.
static PLACEHOLDER_CACHE: std::sync::LazyLock<Vec<String>> = std::sync::LazyLock::new(|| {
    let mut cache = vec![String::new()]; // index 0 unused
    for n in 1..=PLACEHOLDER_CACHE_MAX {
        cache.push(build_placeholders(n));
    }
    cache
});

/// Build a placeholder string without caching (used by both cache init and large n).
fn build_placeholders(n: usize) -> String {
    let mut s = String::with_capacity(n * 4);
    for i in 1..=n {
        if i > 1 {
            s.push(',');
        }
        s.push('?');
        // Fast itoa for small numbers (covers all practical batch sizes)
        if i < 10 {
            s.push((b'0' + i as u8) as char);
        } else if i < 100 {
            s.push((b'0' + (i / 10) as u8) as char);
            s.push((b'0' + (i % 10) as u8) as char);
        } else {
            use std::fmt::Write;
            let _ = write!(s, "{}", i);
        }
    }
    s
}

/// Build a comma-separated list of numbered SQL placeholders: "?1,?2,...,?N".
///
/// Batch sizes up to [`PLACEHOLDER_CACHE_MAX`] are served from a static
/// cache; larger values are built on demand. The cache covers the full
/// caller-facing range — no production call site should fall off it.
pub(crate) fn make_placeholders(n: usize) -> String {
    assert!(
        n <= 100_000,
        "make_placeholders called with unreasonable n={n}"
    );
    if n <= PLACEHOLDER_CACHE_MAX {
        PLACEHOLDER_CACHE[n].clone()
    } else {
        build_placeholders(n)
    }
}
