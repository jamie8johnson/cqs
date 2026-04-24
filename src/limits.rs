//! Library-side env-overridable size limits.
//!
//! Mirrors the spirit of `cli/limits.rs` but lives in the library layer so
//! `parser/`, `store/`, `convert/`, and `nl/fts.rs` can read the same env
//! vars. The CLI-side helpers (rerank pool sizing, diff input cap, daemon
//! response cap) stay in `cli/limits.rs` because they have no library
//! callers.
//!
//! Each helper reads its env var on every call. The cost is negligible
//! (one `getenv` syscall per check) and lets tests flip values inside a
//! single process without rebuilding.

// ============ P3 #102: FTS normalization cap ============

/// Default upper bound on `normalize_for_fts` output bytes. Pathological
/// inputs (multi-MB SQL builders, generated parser code, single huge
/// chunks) can otherwise blow the FTS5 indexer's memory budget. See P3
/// #102 in `docs/audit-findings.md`.
pub(crate) const FTS_NORMALIZE_MAX: usize = 16384;

/// Resolve the FTS cap honoring `CQS_FTS_NORMALIZE_MAX`.
pub(crate) fn fts_normalize_max() -> usize {
    parse_env_usize("CQS_FTS_NORMALIZE_MAX", FTS_NORMALIZE_MAX)
}

// ============ P3 #103: graph edge caps ============

/// Default cap on edges loaded into [`crate::store::Store::get_call_graph`].
/// Protects `cqs impact`, `cqs trace`, and `cqs related` from OOM on
/// adversarially-large `function_calls` tables. See P3 #103.
pub(crate) const CALL_GRAPH_MAX_EDGES: usize = 500_000;

/// Default cap on edges loaded into `Store::get_type_graph`.
pub(crate) const TYPE_GRAPH_MAX_EDGES: usize = 500_000;

/// Resolve the call-graph edge cap honoring `CQS_CALL_GRAPH_MAX_EDGES`.
pub(crate) fn call_graph_max_edges() -> usize {
    parse_env_usize("CQS_CALL_GRAPH_MAX_EDGES", CALL_GRAPH_MAX_EDGES)
}

/// Resolve the type-graph edge cap honoring `CQS_TYPE_GRAPH_MAX_EDGES`.
pub(crate) fn type_graph_max_edges() -> usize {
    parse_env_usize("CQS_TYPE_GRAPH_MAX_EDGES", TYPE_GRAPH_MAX_EDGES)
}

// ============ P3 #104, #105: parser size caps ============

/// Default per-file size cap for tree-sitter parsing (50 MiB). Distinct
/// from `CQS_MAX_FILE_SIZE` (file-discovery gate in `lib.rs::max_file_size`)
/// so per-stage knobs stay independent. See P3 #104.
pub(crate) const PARSER_MAX_FILE_SIZE: u64 = 50 * 1024 * 1024;

/// Default per-chunk byte cap (100 KiB). Chunks larger than this are
/// dropped at parse time before windowing sees them. See P3 #105.
pub(crate) const PARSER_MAX_CHUNK_BYTES: usize = 100_000;

/// Resolve the parser per-file size cap honoring `CQS_PARSER_MAX_FILE_SIZE`.
pub(crate) fn parser_max_file_size() -> u64 {
    parse_env_u64("CQS_PARSER_MAX_FILE_SIZE", PARSER_MAX_FILE_SIZE)
}

/// Resolve the per-chunk byte cap honoring `CQS_PARSER_MAX_CHUNK_BYTES`.
pub(crate) fn parser_max_chunk_bytes() -> usize {
    parse_env_usize("CQS_PARSER_MAX_CHUNK_BYTES", PARSER_MAX_CHUNK_BYTES)
}

// ============ P3 #106, #108: doc converter caps ============

/// Default ceiling on per-archive page count for CHM, web help, and any
/// future multi-page converter. See P3 #106.
pub(crate) const DEFAULT_DOC_MAX_PAGES: usize = 1000;

/// Default `walkdir` depth cap for `convert/mod.rs::convert_directory`.
/// See P3 #108.
pub(crate) const DEFAULT_DOC_MAX_WALK_DEPTH: usize = 50;

/// RM-V1.29-5: per-page byte cap for multi-page converters (CHM, WebHelp).
/// A malicious or pathological archive can ship a single 500MB "page"; the
/// outer `MAX_FILE_SIZE` check only gates the archive, not the extracted
/// pages. Default 10 MiB covers every real-world help page with margin.
pub(crate) const DEFAULT_CONVERT_PAGE_BYTES: u64 = 10 * 1024 * 1024;

/// Resolve the doc page cap honoring `CQS_CONVERT_MAX_PAGES`.
pub(crate) fn doc_max_pages() -> usize {
    parse_env_usize("CQS_CONVERT_MAX_PAGES", DEFAULT_DOC_MAX_PAGES)
}

/// Resolve the walkdir depth cap honoring `CQS_CONVERT_MAX_WALK_DEPTH`.
pub(crate) fn doc_max_walk_depth() -> usize {
    parse_env_usize("CQS_CONVERT_MAX_WALK_DEPTH", DEFAULT_DOC_MAX_WALK_DEPTH)
}

/// RM-V1.29-5: per-page byte cap for CHM / WebHelp page readers, honoring
/// `CQS_CONVERT_PAGE_BYTES`.
pub(crate) fn convert_page_bytes() -> u64 {
    parse_env_u64("CQS_CONVERT_PAGE_BYTES", DEFAULT_CONVERT_PAGE_BYTES)
}

/// SHL-V1.29-10: per-file byte cap for HTML / Markdown converters, honoring
/// `CQS_CONVERT_MAX_FILE_SIZE`. Defaults to 100 MB.
pub(crate) fn convert_file_size() -> u64 {
    const DEFAULT_CONVERT_MAX_FILE_SIZE: u64 = 100 * 1024 * 1024;
    parse_env_u64("CQS_CONVERT_MAX_FILE_SIZE", DEFAULT_CONVERT_MAX_FILE_SIZE)
}

// ============ shared parsing helpers ============

/// Parse a `usize`-shaped env var, falling back to `default` on
/// missing/empty/garbage/zero values.
fn parse_env_usize(key: &str, default: usize) -> usize {
    match std::env::var(key) {
        Ok(v) => v
            .parse::<usize>()
            .ok()
            .filter(|n| *n > 0)
            .unwrap_or(default),
        Err(_) => default,
    }
}

/// Same as [`parse_env_usize`] but for `u64`-shaped byte limits.
fn parse_env_u64(key: &str, default: u64) -> u64 {
    match std::env::var(key) {
        Ok(v) => v.parse::<u64>().ok().filter(|n| *n > 0).unwrap_or(default),
        Err(_) => default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_env_usize_handles_missing_and_garbage() {
        std::env::remove_var("CQS_TEST_LIMITS_USZ");
        assert_eq!(parse_env_usize("CQS_TEST_LIMITS_USZ", 99), 99);
        std::env::set_var("CQS_TEST_LIMITS_USZ", "garbage");
        assert_eq!(parse_env_usize("CQS_TEST_LIMITS_USZ", 99), 99);
        std::env::set_var("CQS_TEST_LIMITS_USZ", "0");
        assert_eq!(parse_env_usize("CQS_TEST_LIMITS_USZ", 99), 99);
        std::env::set_var("CQS_TEST_LIMITS_USZ", "42");
        assert_eq!(parse_env_usize("CQS_TEST_LIMITS_USZ", 99), 42);
        std::env::remove_var("CQS_TEST_LIMITS_USZ");
    }

    #[test]
    fn parse_env_u64_handles_missing_and_garbage() {
        std::env::remove_var("CQS_TEST_LIMITS_U64");
        assert_eq!(parse_env_u64("CQS_TEST_LIMITS_U64", 99), 99);
        std::env::set_var("CQS_TEST_LIMITS_U64", "garbage");
        assert_eq!(parse_env_u64("CQS_TEST_LIMITS_U64", 99), 99);
        std::env::set_var("CQS_TEST_LIMITS_U64", "0");
        assert_eq!(parse_env_u64("CQS_TEST_LIMITS_U64", 99), 99);
        std::env::set_var("CQS_TEST_LIMITS_U64", "42");
        assert_eq!(parse_env_u64("CQS_TEST_LIMITS_U64", 99), 42);
        std::env::remove_var("CQS_TEST_LIMITS_U64");
    }
}
