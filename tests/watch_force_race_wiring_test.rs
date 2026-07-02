//! PR-time wiring tripwires for the `--force`-vs-rebuild race guards.
//!
//! The behavioural unit tests pin the two guard HELPERS in isolation:
//! `drain_completed_rebuild_idle` discards a pending rebuild when the DB was
//! replaced, and `build_hnsw_index_owned` / `build_hnsw_base_index` skip their
//! thread-side save when `skip_save` fires. But a green helper suite says
//! nothing about whether the watch loop and the rebuild worker actually CALL
//! those guards at the right site — deleting a call site leaves every unit test
//! green (its only tripwire would be a dead-code warning, which `#[allow]` or an
//! unrelated use can mask). These source-scan guards assert the wiring itself.
//!
//! Kept as source scans (not loop-driving tests) because driving the real watch
//! loop to the exact idle-tick / mid-build-replace interleaving is racy; the
//! wiring is a structural invariant a cheap scan pins deterministically.

use std::fs;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let p = repo_root().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// (1) The watch loop's idle housekeeping block (`if !flush_due { … }`) must
/// call `drain_completed_rebuild_idle`. Without this the completed-rebuild
/// freshness wedge re-emerges AND the DB-replacement discard never runs on the
/// quiet-repo path (no flush tick fires to reach the flush path's own guard).
///
/// RED if the call site is deleted from the idle block: the helper's unit tests
/// stay green, so this scan is the only tripwire.
#[test]
fn watch_idle_block_calls_drain_completed_rebuild_idle() {
    let src = read("src/cli/watch/mod.rs");
    let lines: Vec<&str> = src.lines().collect();

    // Locate the idle-housekeeping guard. `!flush_due` is the idle arm; the
    // flush arm is `if flush_due(...)` (no bang).
    let idle_guard = lines
        .iter()
        .position(|l| l.contains("if !flush_due("))
        .expect(
            "SITE MOVED: src/cli/watch/mod.rs no longer has an `if !flush_due(` idle guard — \
             update this wiring test to track the idle block's new shape.",
        );

    // The call must appear inside the idle block. Scan a generous window past
    // the guard for a `drain_completed_rebuild_idle(` reference — any occurrence
    // here is a call, since the `fn` definition lives ~1200 lines above and is
    // well outside this window. `contains` (not a bare-name prefix) so a future
    // `let outcome = drain_completed_rebuild_idle(...)` refactor stays green.
    let window_end = (idle_guard + 60).min(lines.len());
    let called = lines[idle_guard..window_end]
        .iter()
        .any(|l| l.contains("drain_completed_rebuild_idle("));

    assert!(
        called,
        "WIRING GONE: the watch loop's `if !flush_due` idle block no longer calls \
         `drain_completed_rebuild_idle`. The idle-tick freshness drain and the \
         DB-replacement discard both run through that call — its unit tests pin the \
         helper, not this call site, so deleting it is invisible to them. Restore the \
         call in src/cli/watch/mod.rs's idle block."
    );
}

/// (2) The rebuild worker must capture the DB identity at spawn and pass the
/// `db_replaced` guard closure into BOTH thread-side build/save helpers, or the
/// worker's orphaned-handle save can overwrite a `--force` replacement's fresh
/// sidecar with a stale build.
///
/// RED if the capture or either `Some(&db_replaced)` argument is dropped.
#[test]
fn rebuild_worker_wires_db_replaced_guard_into_both_saves() {
    let src = read("src/cli/watch/rebuild.rs");

    assert!(
        src.contains("let db_id_at_spawn = db_file_identity(&index_path);"),
        "WIRING GONE: src/cli/watch/rebuild.rs no longer captures the spawn-time DB identity \
         (`let db_id_at_spawn = db_file_identity(&index_path);`). The thread-side save guard \
         has no reference to compare against."
    );
    assert!(
        src.contains("let db_replaced = || db_file_identity(&index_path) != db_id_at_spawn;"),
        "WIRING GONE: src/cli/watch/rebuild.rs no longer builds the `db_replaced` guard closure."
    );

    let passes = src.matches("Some(&db_replaced)").count();
    assert!(
        passes >= 2,
        "WIRING GONE: expected the `db_replaced` guard passed to BOTH thread-side saves \
         (build_hnsw_index_owned + build_hnsw_base_index) in src/cli/watch/rebuild.rs; \
         found {passes} `Some(&db_replaced)` argument(s). A save left unguarded can overwrite \
         a `--force` replacement's fresh sidecar."
    );
}
