//! SQL placeholder generation, caching, and batch-size derivation.
//!
//! ## SQLite variable limit
//!
//! cqs requires SQLite 3.35+ (for RETURNING), so `SQLITE_MAX_VARIABLE_NUMBER`
//! is always 32766. The [`max_rows_per_statement`] helper centralizes the
//! batch-size derivation so call sites don't re-derive the constant (and
//! don't fall back to the old 999-derived sizes, which produce 10-30× more
//! SQL statements than necessary).

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

/// SQLite's `SQLITE_MAX_VARIABLE_NUMBER`.
/// Single source of truth — all batch-size derivations reference this.
pub const SQLITE_MAX_VARIABLES: usize = 32766;

/// Generic headroom so a future caller adding one more bind variable
/// doesn't instantly trip the limit. NOT sized to absorb a full extra
/// column; adding a new column requires updating `vars_per_row` at the
/// call site.
pub const SAFETY_MARGIN_VARS: usize = 300;

/// Derive the maximum rows per INSERT/DELETE statement given the number
/// of bind variables per row. Centralizes the `(LIMIT - MARGIN) / N`
/// derivation so call sites don't inline it.
///
/// For single-bind queries (e.g. `WHERE id IN (?, ?, ...)`), pass
/// `vars_per_row = 1`. For multi-column INSERTs, pass the column count.
pub const fn max_rows_per_statement(vars_per_row: usize) -> usize {
    (SQLITE_MAX_VARIABLES - SAFETY_MARGIN_VARS) / vars_per_row
}

/// Maximum batch size that is pre-built and cached at startup.
///
/// Sized exactly to cover the caller-facing max
/// (`max_rows_per_statement(1) = SQLITE_MAX_VARIABLES - SAFETY_MARGIN_VARS
/// = 32466`) so single-bind batches up to that size never fall off the
/// cache and re-build the ~120KB placeholder string. The extra ~22k strings
/// cost ~1-2MB at startup in exchange for zero-alloc on the hot path.
const PLACEHOLDER_CACHE_MAX: usize = SQLITE_MAX_VARIABLES - SAFETY_MARGIN_VARS;

/// Lazy per-size placeholder cache. Each slot holds a [`std::sync::OnceLock`]
/// that builds its specific placeholder string on first access — so a
/// session that uses batches of size 500 and 8116 only ever builds two
/// strings, not 32,466.
///
/// **Eager pre-building would be a 30-second startup tax.** Building every
/// string from 1..=32,466 up front is O(n²) total chars —
/// `sum(6n for n in 1..=32466) ≈ 3 × 10⁹` chars — which on Linux /tmp takes
/// ~30 s, triggered on the first DB write (the first call to
/// [`make_placeholders`]) and easily mistaken for a connection-pool hang.
/// The lazy design avoids paying for sizes never used.
///
/// The `Vec<OnceLock<String>>` keeps O(1) lookup (index into the Vec) and
/// zero-alloc-on-hit semantics (`Cow::Borrowed` from the owned `String`
/// inside the `OnceLock`).
///
/// Memory: `Vec<OnceLock<String>>` of length 32,467 ≈ 520 KB of metadata
/// upfront — microseconds to allocate.
static PLACEHOLDER_CACHE: std::sync::LazyLock<Vec<std::sync::OnceLock<String>>> =
    std::sync::LazyLock::new(|| {
        // `Vec::with_capacity` + `resize_with` is the cheapest way to get a
        // fixed-size Vec of distinct OnceLock instances (`vec![item; N]`
        // requires Clone, which OnceLock isn't).
        let mut v = Vec::with_capacity(PLACEHOLDER_CACHE_MAX + 1);
        v.resize_with(PLACEHOLDER_CACHE_MAX + 1, std::sync::OnceLock::new);
        v
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
/// per-size [`std::sync::OnceLock`] cache as `Cow::Borrowed(&'static str)`;
/// larger values build a fresh `String` on demand and return `Cow::Owned`.
/// The cache covers the full caller-facing range — no production call site
/// should fall off it.
///
/// A cache hit returns a `&'static str` borrow via `Cow::Borrowed`, avoiding
/// the ~4KB memcpy a 500-id batch would cost if it re-allocated the full
/// placeholder string per call — measurable allocator pressure on a hot
/// reindex or batch-search loop.
///
/// `PLACEHOLDER_CACHE_MAX` is bound to `SQLITE_MAX_VARIABLES -
/// SAFETY_MARGIN_VARS` so large batches don't miss the cache.
///
/// Each entry is built lazily (per-size [`OnceLock`]); see
/// [`PLACEHOLDER_CACHE`] for why eager build was a 30-second startup tax.
pub(crate) fn make_placeholders(n: usize) -> std::borrow::Cow<'static, str> {
    assert!(
        n <= 100_000,
        "make_placeholders called with unreasonable n={n}"
    );
    if n <= PLACEHOLDER_CACHE_MAX {
        std::borrow::Cow::Borrowed(
            PLACEHOLDER_CACHE[n]
                .get_or_init(|| build_placeholders(n))
                .as_str(),
        )
    } else {
        std::borrow::Cow::Owned(build_placeholders(n))
    }
}

/// Build a comma-separated list of numbered SQL placeholders starting at
/// `start`: `"?{start},?{start+1},...,?{start+n-1}"`.
///
/// Builds the list without the intermediate `n` `String`s + `Vec` an inline
/// `(0..n).map(|i| format!("?{}", base + i + 1)).collect().join(",")` would
/// allocate. For `start = 1` this routes to the cached [`make_placeholders`]
/// (zero allocation on cache hit).
pub(crate) fn make_placeholders_offset(n: usize, start: usize) -> std::borrow::Cow<'static, str> {
    assert!(
        n <= 100_000,
        "make_placeholders_offset called with unreasonable n={n}"
    );
    if start == 1 {
        return make_placeholders(n);
    }
    if n == 0 {
        return std::borrow::Cow::Borrowed("");
    }
    // Offset variants are not cached — a `(n, start)` keyed cache would
    // explode for large filter chains. Build once on demand without the
    // intermediate `Vec<String>` the inline pattern allocated.
    let mut s = String::with_capacity(n * 5);
    use std::fmt::Write;
    for i in 0..n {
        if i > 0 {
            s.push(',');
        }
        let _ = write!(&mut s, "?{}", start + i);
    }
    std::borrow::Cow::Owned(s)
}
