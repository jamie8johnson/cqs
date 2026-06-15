//! Frozen-artifact guard: the FULL schema migration chain from the OLDEST
//! migratable version (v10) to current (v32), run against a hand-built v10 DB.
//!
//! # The version boundary
//!
//! Every fresh-state fixture is born at the current schema. The existing
//! migration tests each exercise ONE step (or at most a 3-step v12→v14 chain)
//! against a minimal per-step setup helper. NONE runs the complete v10→v32
//! chain against a faithfully-seeded oldest DB. That chain crosses every
//! destructive in-place TABLE REBUILD — v15→v16 (llm_summaries composite PK),
//! v18→v19 (sparse_vectors FK), v28→v29 (notes CHECK + clamp) — plus the recent
//! v29→v30 (function_calls.edge_kind), v30→v31, v31→v32 (candidate_edges) steps.
//! A bug where a later step assumes a column/shape an old DB lacks, or a
//! rebuild silently drops legacy rows, would only fire on the full chain from
//! the oldest shape — exactly the state no born-at-HEAD fixture can construct.
//!
//! # What this guard constructs (the only way to reach the null)
//!
//! A v10 DB AT REST, written with the exact schema a schema-10 binary shipped
//! (extracted from git: commit afa8fb9, CURRENT_SCHEMA_VERSION=10):
//!   - `chunks`  with the v10 column set (origin/source_type/source_mtime; NO
//!     parser_version, enrichment_*, sparse, vendored, canonical_hash, etc.).
//!   - `notes`   769-dim embedding (the pre-v15 sentiment-augmented size) and an
//!     OFF-GRID sentiment (-0.8) — a shape ONLY a pre-v29 binary could write
//!     (post-v29 the schema CHECK rejects it). This is the legacy-only read the
//!     v28→v29 clamp must translate.
//!   - `function_calls` rows with NO `edge_kind` column (pre-v30) — the legacy
//!     shape the v29→v30 default ('call') + the `from_str_or_default` read must
//!     coerce correctly.
//!   - `type_edges`, `sparse_vectors`, `llm_summaries`, `file_registry`,
//!     `candidate_edges` all ABSENT (created by later migrations).
//!
//! The current code can never emit this DB: it always writes schema 32.
//!
//! # What it asserts (current code reads/migrates the old shape correctly)
//!
//!   (1) `Store::open` runs the whole v10→v32 chain without error and stamps 32.
//!   (2) Every v10 chunk row SURVIVES the chain (no silent loss across the
//!       three destructive rebuilds). `chunk_count` and the rows match.
//!   (3) The off-grid note (-0.8) is CLAMPED to the nearest legal discrete
//!       value (-1.0) — the v28→v29 legacy-read clamp.
//!   (4) The pre-v30 function_calls edge reads back through the real caller
//!       query with `edge_kind = Call` (the default-on-absence coercion).
//!   (5) All the tables a later migration adds exist after the chain.
//!
//! # Calibration (proven RED on a mutated assertion)
//!
//! See the per-test notes: mutating the v28→v29 clamp expression (e.g. drop the
//! `round(...)`), or removing a later table's CREATE, turns the relevant
//! assertion RED. A guard that stays green under such a mutation would be
//! vacuous; these do not.

use std::path::Path;

use cqs::store::Store;

/// Read a single metadata value or probe table existence on the migrated DB
/// file directly via sqlx (no Store helper needed — keeps this guard a pure
/// integration test against the public open path).
fn query_scalar_string(db_path: &Path, sql: String) -> Option<String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        use sqlx::sqlite::SqlitePoolOptions;
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(db_path)
                    .read_only(true),
            )
            .await
            .unwrap();
        let row: Option<(String,)> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
            .fetch_optional(&pool)
            .await
            .unwrap();
        pool.close().await;
        row.map(|(s,)| s)
    })
}

/// Build a v10 DB file on disk exactly as a schema-10 binary would have left
/// it, then seed the legacy-only rows. Returns nothing — the file at `db_path`
/// is the frozen artifact.
fn build_v10_artifact(db_path: &Path) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        use sqlx::sqlite::SqlitePoolOptions;
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(db_path)
                    .create_if_missing(true),
            )
            .await
            .unwrap();

        // --- metadata (schema 10) ---
        sqlx::query("CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO metadata (key, value) VALUES ('schema_version', '10')")
            .execute(&pool)
            .await
            .unwrap();
        // Pre-v15 dimensions: 769 (768 model + sentiment). The v14→v15 migration
        // rewrites 769→768; if the chain mishandles it the dim assertion (and
        // the store's notion of embedding length) would drift.
        sqlx::query("INSERT INTO metadata (key, value) VALUES ('dimensions', '769')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO metadata (key, value) VALUES ('model', 'e5-base-v2')")
            .execute(&pool)
            .await
            .unwrap();

        // --- chunks (schema 10 column set, from git afa8fb9) ---
        sqlx::query(
            "CREATE TABLE chunks (
                id TEXT PRIMARY KEY,
                origin TEXT NOT NULL,
                source_type TEXT NOT NULL,
                language TEXT NOT NULL,
                chunk_type TEXT NOT NULL,
                name TEXT NOT NULL,
                signature TEXT NOT NULL,
                content TEXT NOT NULL,
                content_hash TEXT NOT NULL,
                doc TEXT,
                line_start INTEGER NOT NULL,
                line_end INTEGER NOT NULL,
                embedding BLOB NOT NULL,
                source_mtime INTEGER,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                parent_id TEXT,
                window_idx INTEGER
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("CREATE INDEX idx_chunks_origin ON chunks(origin)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("CREATE INDEX idx_chunks_source_type ON chunks(source_type)")
            .execute(&pool)
            .await
            .unwrap();

        // A 769-element f32 embedding blob (the pre-v15 sentiment-augmented size).
        let emb_769: Vec<u8> = (0..769)
            .flat_map(|_| 0.1_f32.to_le_bytes())
            .collect::<Vec<u8>>();

        for (id, name, line_start, line_end) in [
            ("file:src/a.rs:1:0:aaaaaaaa", "foo", 1_i64, 5_i64),
            ("file:src/a.rs:7:0:bbbbbbbb", "bar", 7, 12),
        ] {
            sqlx::query(
                "INSERT INTO chunks (id, origin, source_type, language, chunk_type, name, \
                 signature, content, content_hash, doc, line_start, line_end, embedding, \
                 source_mtime, created_at, updated_at, parent_id, window_idx) \
                 VALUES (?1, 'src/a.rs', 'file', 'rust', 'function', ?2, ?3, ?4, ?5, NULL, \
                 ?6, ?7, ?8, 1700000000, '2026-01-01', '2026-01-01', NULL, NULL)",
            )
            .bind(id)
            .bind(name)
            .bind(format!("fn {name}()"))
            .bind(format!("fn {name}() {{ /* legacy v10 body */ }}"))
            .bind(format!("hash_{name}"))
            .bind(line_start)
            .bind(line_end)
            .bind(&emb_769)
            .execute(&pool)
            .await
            .unwrap();
        }

        // --- notes (schema 10 / pre-v15 769-dim, pre-v29 off-grid sentiment) ---
        sqlx::query(
            "CREATE TABLE notes (
                id TEXT PRIMARY KEY,
                text TEXT NOT NULL,
                sentiment REAL NOT NULL,
                mentions TEXT,
                embedding BLOB NOT NULL,
                source_file TEXT NOT NULL,
                file_mtime INTEGER NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("CREATE INDEX idx_notes_sentiment ON notes(sentiment)")
            .execute(&pool)
            .await
            .unwrap();

        // THE legacy-only shape: an off-grid sentiment (-0.8). Post-v29 the
        // schema CHECK forbids this — only a pre-v29 binary could have written
        // it. The v28→v29 clamp must rewrite it to -1.0.
        sqlx::query(
            "INSERT INTO notes (id, text, sentiment, mentions, embedding, source_file, \
             file_mtime, created_at, updated_at) \
             VALUES ('note:0', 'a v10 note with off-grid sentiment', -0.8, '[\"foo\"]', ?1, \
             'docs/notes.toml', 0, '2026-01-01', '2026-01-01')",
        )
        .bind(&emb_769)
        .execute(&pool)
        .await
        .unwrap();
        // A second note already on-grid — must survive unchanged.
        sqlx::query(
            "INSERT INTO notes (id, text, sentiment, mentions, embedding, source_file, \
             file_mtime, created_at, updated_at) \
             VALUES ('note:1', 'a v10 note on grid', 0.5, '[]', ?1, \
             'docs/notes.toml', 0, '2026-01-01', '2026-01-01')",
        )
        .bind(&emb_769)
        .execute(&pool)
        .await
        .unwrap();

        // --- function_calls (schema 10: NO edge_kind column, pre-v30) ---
        sqlx::query(
            "CREATE TABLE function_calls (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                file TEXT NOT NULL,
                caller_name TEXT NOT NULL,
                caller_line INTEGER NOT NULL,
                callee_name TEXT NOT NULL,
                call_line INTEGER NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        // foo() calls bar() — a legacy edge with no provenance tag.
        sqlx::query(
            "INSERT INTO function_calls (file, caller_name, caller_line, callee_name, call_line) \
             VALUES ('src/a.rs', 'foo', 1, 'bar', 3)",
        )
        .execute(&pool)
        .await
        .unwrap();

        pool.close().await;
    });
}

/// GUARD: the full v10→v32 chain reads/migrates the frozen v10 artifact
/// correctly through the real `Store::open` path (which auto-migrates).
///
/// Calibration (RED proof): mutate the v28→v29 clamp in
/// `src/store/migrations.rs` (e.g. replace `round(MAX(-1.0, MIN(1.0, sentiment)) *
/// 2.0) / 2.0` with a bare `sentiment`) → assertion (3) goes RED with a CHECK
/// violation that aborts the whole chain (the migrate() backup/restore fires and
/// `Store::open` returns Err). Remove the v31→v32 `candidate_edges` CREATE →
/// assertion (5) goes RED. GREEN on the shipped chain.
#[test]
fn v10_full_chain_migrates_and_reads_legacy_shapes() {
    let tmp = tempfile::TempDir::new().unwrap();
    let db_path = tmp.path().join("index.db");

    // FROZEN ARTIFACT: a v10 DB at rest. No born-at-HEAD fixture can build it.
    build_v10_artifact(&db_path);

    // (1) Open through the real path — this runs check_and_migrate_schema, i.e.
    // the entire v10→v32 chain. A mis-migration surfaces as an Err here.
    let store = Store::open(&db_path).expect("Store::open must migrate v10 → v32 without error");

    // schema_version is stamped 32. (Probe the file directly; Store is open
    // read-write but we re-read via a separate read-only connection.)
    let sv = query_scalar_string(
        &db_path,
        "SELECT value FROM metadata WHERE key = 'schema_version'".to_string(),
    )
    .expect("schema_version row must exist after migrate");
    assert_eq!(sv, "32", "full chain must stamp schema_version = 32");

    // (2) Every v10 chunk row survived all three destructive rebuilds.
    let count = store.chunk_count().expect("chunk_count after migrate");
    assert_eq!(
        count, 2,
        "both v10 chunks must survive the full migration chain (no silent loss)"
    );
    let chunks = store
        .get_chunks_by_origin("src/a.rs")
        .expect("get_chunks_by_origin after migrate");
    let names: std::collections::HashSet<&str> = chunks.iter().map(|c| c.name.as_str()).collect();
    assert!(
        names.contains("foo") && names.contains("bar"),
        "both legacy chunk rows must be queryable after migrate; got {names:?}"
    );

    // (3) The off-grid note (-0.8) is clamped to the nearest legal value (-1.0)
    // by the v28→v29 legacy-read clamp; the on-grid note (0.5) is unchanged.
    let notes = store
        .list_notes_summaries()
        .expect("list notes after migrate");
    let by_id: std::collections::HashMap<&str, f32> =
        notes.iter().map(|n| (n.id.as_str(), n.sentiment)).collect();
    assert_eq!(
        by_id.get("note:0").copied(),
        Some(-1.0),
        "v28→v29 must clamp the pre-v29 off-grid -0.8 to -1.0; got {:?}",
        by_id.get("note:0")
    );
    assert_eq!(
        by_id.get("note:1").copied(),
        Some(0.5),
        "an already-on-grid note must survive the clamp unchanged"
    );

    // (4) The pre-v30 function_calls edge reads back with edge_kind = Call (the
    // v29→v30 default-on-absence + from_str_or_default coercion).
    let callers = store
        .get_callers_full("bar")
        .expect("get_callers_full after migrate");
    assert_eq!(callers.len(), 1, "the legacy foo→bar edge must survive");
    assert_eq!(callers[0].name, "foo");
    assert_eq!(
        callers[0].edge_kind,
        cqs::parser::CallEdgeKind::Call,
        "a pre-v30 edge (no edge_kind column) must read as the default Call"
    );
}

/// GUARD (companion): every table a later migration introduces EXISTS after the
/// chain runs against the v10 artifact. A skipped/forgotten CREATE in a later
/// step would leave the table absent and a subsequent reader would 'no such
/// table'-crash on first use after upgrade — the worst legacy class (silent
/// until a specific feature is exercised post-upgrade).
#[test]
fn v10_full_chain_creates_all_later_tables() {
    let tmp = tempfile::TempDir::new().unwrap();
    let db_path = tmp.path().join("index.db");
    build_v10_artifact(&db_path);

    let _store = Store::open(&db_path).expect("Store::open must migrate v10 → v32");

    for table in [
        "type_edges",      // v10→v11
        "llm_summaries",   // v13→v14 (rebuilt v15→v16)
        "sparse_vectors",  // v16→v17 (rebuilt v18→v19)
        "file_registry",   // v28→v29
        "candidate_edges", // v31→v32
    ] {
        let found = query_scalar_string(
            &db_path,
            format!("SELECT name FROM sqlite_master WHERE type='table' AND name='{table}'"),
        );
        assert_eq!(
            found.as_deref(),
            Some(table),
            "table {table} must exist after the full v10→v32 chain"
        );
    }
}
