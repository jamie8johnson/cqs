//! SQL placeholder generation and caching.

/// Maximum batch size that is pre-built and cached at startup.
/// All observed batch sizes (55, 100, 190, 200, 250, 300, 500, 900) fall within this range.
const PLACEHOLDER_CACHE_MAX: usize = 999;

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
/// Common batch sizes (1-999) are served from a static cache; larger values are built on demand.
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
