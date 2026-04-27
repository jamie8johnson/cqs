//! Audit P3 #116 — `cmd_cache` (stats/clear/prune) integration tests.
//!
//! `cqs cache <subcmd>` operates on the global embedding cache at
//! `~/.cache/cqs/embeddings.db` (`EmbeddingCache::default_path()`). The
//! handler in `src/cli/commands/infra/cache_cmd.rs` opens the cache,
//! delegates to `EmbeddingCache::{stats,clear,prune_older_than}`, and
//! emits envelope JSON via `crate::cli::json_envelope::emit_json`. There
//! are no integration tests today — only the cache library has direct
//! tests in `src/cache.rs`, and only the `--json cache stats` envelope
//! shape is pinned (in `cli_envelope_test.rs`).
//!
//! Isolation: every test sets `HOME` to a per-test tempdir so the cache
//! file lives at `<tempdir>/.cache/cqs/embeddings.db` and concurrent test
//! runs don't collide on the dev machine's real `~/.cache/cqs/`. We also
//! set `CQS_NO_DAEMON=1` so the daemon-forward path doesn't short-circuit
//! to a running daemon's view of the real cache.
//!
//! These tests run without loading the embedder (they only open the
//! sqlite cache), so they are NOT gated `slow-tests` — they execute on
//! every PR run.

use assert_cmd::Command;
use serial_test::serial;
use tempfile::TempDir;

fn cqs() -> Command {
    #[allow(deprecated)]
    Command::cargo_bin("cqs").expect("Failed to find cqs binary")
}

/// Force CLI mode (no daemon) so a daemon attached to the dev machine's
/// real cache can't intercept the call.
fn cqs_no_daemon() -> Command {
    let mut c = cqs();
    c.env("CQS_NO_DAEMON", "1");
    c
}

/// `cqs cache stats --json` on an empty cache emits the envelope and the
/// inner stats payload with all numeric fields zeroed out. Pins:
/// - envelope shape (`data` object, `error` null, `version`=1)
/// - `total_entries == 0`, `total_size_bytes == 0`, `total_size_mb == 0.0`
/// - `unique_models == 0`
/// - `total_size_mb` is numeric (not string — P1 #11 already covered in
///   `cli_envelope_test.rs`, this re-pins it on the fresh-cache shape).
#[test]
#[serial]
fn test_cache_stats_empty_cache_emits_zero_envelope() {
    let dir = TempDir::new().expect("tempdir");

    // Run from tempdir so `find_project_root()` doesn't escape into the
    // surrounding cqs checkout (which has `.cqs/` and a populated
    // per-project cache after PR #1105). `current_dir` MUST be set or the
    // project-scoped cache_path resolution at cache_cmd.rs:68-76 picks up
    // the parent repo's cache.
    let output = cqs_no_daemon()
        .args(["cache", "stats", "--json"])
        .current_dir(dir.path())
        .env("HOME", dir.path())
        .env("XDG_DATA_HOME", dir.path())
        .env("XDG_CACHE_HOME", dir.path())
        .output()
        .expect("cqs cache stats --json failed to spawn");

    assert!(
        output.status.success(),
        "stats on empty cache should succeed. stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("envelope JSON parse failed: {e}\nstdout={stdout}"));

    // Envelope shape
    assert_eq!(parsed["version"], 1);
    assert!(parsed["error"].is_null());
    assert!(parsed["data"].is_object(), "data must be object: {stdout}");

    // Inner stats shape — empty cache has zero entries / models. Note
    // `total_size_bytes` is the SQLite file size, which is non-zero even
    // for an empty WAL-mode db (the page header takes ~16 KiB). Pin the
    // shape and the count fields, but only assert size is numeric + >= 0.
    assert_eq!(
        parsed["data"]["total_entries"], 0,
        "fresh cache should have 0 entries"
    );
    let bytes = parsed["data"]["total_size_bytes"]
        .as_u64()
        .expect("total_size_bytes must be numeric");
    // bytes is u64, always >= 0; we only assert numeric and < 1 MB for
    // a freshly-opened cache. P2.16 dropped `total_size_mb` (bytes is
    // canonical), so we no longer assert that field exists.
    assert!(
        bytes < 1024 * 1024,
        "fresh-cache file should be tiny (<1 MB), got {bytes} bytes"
    );
    assert!(
        parsed["data"].get("total_size_mb").is_none(),
        "P2.16: total_size_mb should be removed; bytes is canonical. got: {}",
        parsed["data"]
    );
    assert_eq!(parsed["data"]["unique_models"], 0);
}

/// `cqs cache clear --json` on an empty cache returns `deleted: 0`.
/// Pins the no-op behaviour through `cache_clear` at `cache_cmd.rs:93-110`
/// — passing no `--model` means `clear(None)` → all rows.
#[test]
#[serial]
fn test_cache_clear_empty_cache_returns_zero_deleted() {
    let dir = TempDir::new().expect("tempdir");

    let output = cqs_no_daemon()
        .args(["cache", "clear", "--json"])
        .current_dir(dir.path())
        .env("HOME", dir.path())
        .env("XDG_DATA_HOME", dir.path())
        .env("XDG_CACHE_HOME", dir.path())
        .output()
        .expect("cqs cache clear failed to spawn");

    assert!(
        output.status.success(),
        "clear on empty cache should succeed. stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("envelope JSON parse failed: {e}\nstdout={stdout}"));

    assert_eq!(parsed["version"], 1);
    assert!(parsed["error"].is_null());
    assert_eq!(
        parsed["data"]["deleted"], 0,
        "empty cache → clear deletes 0 rows"
    );
    // model field is null when `--model` is omitted (Option<&str> serializes
    // to JSON null via Serialize).
    assert!(
        parsed["data"]["model"].is_null(),
        "model must be null when --model omitted, got: {}",
        parsed["data"]["model"]
    );
}

/// `cqs cache prune 0 --json` on an empty cache returns `pruned: 0` and
/// echoes `older_than_days: 0`. Pins the prune path through
/// `cache_prune` at `cache_cmd.rs:112-129` — `prune_older_than(0)` should
/// match every entry but on an empty cache returns 0.
#[test]
#[serial]
fn test_cache_prune_zero_days_on_empty_returns_zero_pruned() {
    let dir = TempDir::new().expect("tempdir");

    let output = cqs_no_daemon()
        .args(["cache", "prune", "0", "--json"])
        .current_dir(dir.path())
        .env("HOME", dir.path())
        .env("XDG_DATA_HOME", dir.path())
        .env("XDG_CACHE_HOME", dir.path())
        .output()
        .expect("cqs cache prune failed to spawn");

    assert!(
        output.status.success(),
        "prune on empty cache should succeed. stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("envelope JSON parse failed: {e}\nstdout={stdout}"));

    assert_eq!(parsed["version"], 1);
    assert!(parsed["error"].is_null());
    assert_eq!(
        parsed["data"]["pruned"], 0,
        "empty cache → prune 0 days should still report 0 pruned"
    );
    assert_eq!(
        parsed["data"]["older_than_days"], 0,
        "older_than_days must echo the requested value"
    );
}
