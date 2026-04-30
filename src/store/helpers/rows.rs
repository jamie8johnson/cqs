//! Database row types for SQLite result mapping.

/// Clamp i64 to valid u32 line number range (1-indexed)
///
/// SQLite returns i64, but line numbers are u32 and 1-indexed.
/// This safely clamps to avoid truncation issues on extreme values,
/// with minimum 1 since line 0 is invalid in 1-indexed systems.
#[inline]
pub fn clamp_line_number(n: i64) -> u32 {
    n.clamp(1, u32::MAX as i64) as u32
}

/// Lightweight candidate row for scoring (PF-5).
///
/// Contains only the fields needed for candidate scoring and filtering --
/// excludes heavy `content`, `doc`, `signature`, `line_start`, `line_end`
/// fields. Full content is loaded only for top-k survivors via `ChunkRow`.
#[derive(Clone)]
pub(crate) struct CandidateRow {
    pub id: String,
    pub name: String,
    pub origin: String,
    pub language: String,
    pub chunk_type: String,
}

impl CandidateRow {
    /// Construct from a SQLite row containing columns:
    /// id, name, origin, language, chunk_type
    pub(crate) fn from_row(row: &sqlx::sqlite::SqliteRow) -> Self {
        use sqlx::Row;
        CandidateRow {
            id: row.get("id"),
            name: row.get("name"),
            origin: row.get("origin"),
            language: row.get("language"),
            chunk_type: row.get("chunk_type"),
        }
    }
}

/// Raw row from chunks table (crate-internal, used by search module)
#[derive(Clone)]
pub(crate) struct ChunkRow {
    pub id: String,
    pub origin: String,
    pub language: String,
    pub chunk_type: String,
    pub name: String,
    pub signature: String,
    pub content: String,
    pub doc: Option<String>,
    pub line_start: u32,
    pub line_end: u32,
    pub content_hash: String,
    pub window_idx: Option<i32>,
    pub parent_id: Option<String>,
    pub parent_type_name: Option<String>,
    /// Parser logic stamp (P2 #29). 0 means either pre-v21 or never written;
    /// `try_get` keeps existing SELECTs that omit the column working.
    pub parser_version: u32,
    /// v24: true if origin matched a vendored-path prefix at index time
    /// (#1221). `try_get` so pre-v24 SELECTs that omit the column still
    /// construct a valid row (defaults to false).
    pub vendored: bool,
}

impl ChunkRow {
    /// Construct from a SQLite row containing columns:
    /// id, origin, language, chunk_type, name, signature, content, doc, line_start, line_end, parent_id, parent_type_name
    /// (parser_version is read via `try_get` so SELECTs that omit it still work).
    pub(crate) fn from_row(row: &sqlx::sqlite::SqliteRow) -> Self {
        use sqlx::Row;
        ChunkRow {
            id: row.get("id"),
            origin: row.get("origin"),
            language: row.get("language"),
            chunk_type: row.get("chunk_type"),
            name: row.get("name"),
            signature: row.get("signature"),
            content: row.get("content"),
            doc: row.get("doc"),
            line_start: clamp_line_number(row.get::<i64, _>("line_start")),
            line_end: clamp_line_number(row.get::<i64, _>("line_end")),
            content_hash: row.get("content_hash"),
            window_idx: row.try_get("window_idx").unwrap_or(None),
            parent_id: row.get("parent_id"),
            parent_type_name: row.get("parent_type_name"),
            // try_get so SELECT lists that don't pull parser_version still
            // construct a valid row — most search/read paths don't need it.
            parser_version: row
                .try_get::<i64, _>("parser_version")
                .map(|v| v.max(0).min(u32::MAX as i64) as u32)
                .unwrap_or(0),
            vendored: row
                .try_get::<i64, _>("vendored")
                .map(|v| v != 0)
                .unwrap_or(false),
        }
    }

    /// Construct from a SQLite row that omits content/doc columns.
    ///
    /// Used by queries (e.g., `find_test_chunks_async`) that SELECT only lightweight
    /// metadata columns. Missing columns default: content/content_hash -> empty string,
    /// doc/window_idx -> None.
    pub(crate) fn from_row_lightweight(row: &sqlx::sqlite::SqliteRow) -> Self {
        use sqlx::Row;
        ChunkRow {
            id: row.get("id"),
            origin: row.get("origin"),
            language: row.get("language"),
            chunk_type: row.get("chunk_type"),
            name: row.get("name"),
            signature: row.get("signature"),
            content: String::new(),
            doc: None,
            line_start: clamp_line_number(row.get::<i64, _>("line_start")),
            line_end: clamp_line_number(row.get::<i64, _>("line_end")),
            content_hash: String::new(),
            window_idx: None,
            parent_id: row.get("parent_id"),
            parent_type_name: row.get("parent_type_name"),
            parser_version: 0,
            vendored: row
                .try_get::<i64, _>("vendored")
                .map(|v| v != 0)
                .unwrap_or(false),
        }
    }

    /// Construct from a `LightChunk` plus separately-fetched content and doc.
    ///
    /// Used by `find_dead_code` where Phase 1 loads lightweight metadata and Phase 2
    /// fetches content/doc only for candidates that pass filtering.
    pub(crate) fn from_light_chunk(
        light: crate::store::calls::LightChunk,
        content: String,
        doc: Option<String>,
    ) -> Self {
        ChunkRow {
            id: light.id,
            origin: light.file.to_string_lossy().into_owned(),
            language: light.language.to_string(),
            chunk_type: light.chunk_type.to_string(),
            name: light.name,
            signature: light.signature,
            content,
            doc,
            line_start: light.line_start,
            line_end: light.line_end,
            content_hash: String::new(),
            window_idx: None,
            parent_id: None,
            parent_type_name: None,
            parser_version: 0,
            vendored: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clamp_line_number_normal() {
        assert_eq!(clamp_line_number(1), 1);
        assert_eq!(clamp_line_number(100), 100);
    }

    #[test]
    fn test_clamp_line_number_negative() {
        // Line numbers are 1-indexed, so negative/zero clamps to 1
        assert_eq!(clamp_line_number(-1), 1);
        assert_eq!(clamp_line_number(-1000), 1);
        assert_eq!(clamp_line_number(0), 1);
    }

    #[test]
    fn test_clamp_line_number_overflow() {
        assert_eq!(clamp_line_number(i64::MAX), u32::MAX);
        assert_eq!(clamp_line_number(u32::MAX as i64 + 1), u32::MAX);
    }
}
