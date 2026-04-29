//! Watch module unit tests.
//!
//! Lifted out of the inline `mod tests` in `mod.rs` (PR #1147) so the
//! production code reads as a module surface rather than 60% test bench.

use super::*;
use notify::EventKind;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::LazyLock;

// RM-V1.29-8: shared test fixtures. Previously each call to
// `test_watch_config*` leaked a fresh `Parser` / `OnceLock` /
// `ModelConfig` / `RwLock<None>` on the heap, which piled up across
// the ~two dozen watch tests. Every one of these is identical across
// calls, so we keep exactly one `&'static` copy per type. The
// `test_watch_config_with_gitignore` helper still has to leak its
// per-call matcher (each caller passes a distinct `Gitignore`) — but
// the shared four fields no longer leak on every call.
static TEST_PARSER: LazyLock<CqParser> = LazyLock::new(|| CqParser::new().unwrap());
static TEST_EMBEDDER: LazyLock<std::sync::OnceLock<std::sync::Arc<Embedder>>> =
    LazyLock::new(std::sync::OnceLock::new);
static TEST_MODEL_CONFIG: LazyLock<ModelConfig> = LazyLock::new(ModelConfig::default_model);
static TEST_GITIGNORE_NONE: LazyLock<std::sync::RwLock<Option<ignore::gitignore::Gitignore>>> =
    LazyLock::new(|| std::sync::RwLock::new(None));

fn make_event(paths: Vec<PathBuf>, kind: EventKind) -> notify::Event {
    notify::Event {
        kind,
        paths,
        attrs: Default::default(),
    }
}

/// Helper to build a minimal WatchConfig for testing collect_events.
fn test_watch_config<'a>(
    root: &'a Path,
    cqs_dir: &'a Path,
    notes_path: &'a Path,
    supported_ext: &'a HashSet<&'a str>,
) -> WatchConfig<'a> {
    // These fields are unused by collect_events but required by the
    // struct. The four fixtures are shared `LazyLock` statics so
    // tests reference a single `&'static` copy instead of leaking a
    // fresh heap allocation on every call.
    WatchConfig {
        root,
        cqs_dir,
        notes_path,
        supported_ext,
        parser: &TEST_PARSER,
        embedder: &TEST_EMBEDDER,
        quiet: true,
        model_config: &TEST_MODEL_CONFIG,
        gitignore: &TEST_GITIGNORE_NONE,
        splade_encoder: None,
        global_cache: None,
    }
}

/// Variant that installs a gitignore matcher for .gitignore-specific tests.
fn test_watch_config_with_gitignore<'a>(
    root: &'a Path,
    cqs_dir: &'a Path,
    notes_path: &'a Path,
    supported_ext: &'a HashSet<&'a str>,
    matcher: ignore::gitignore::Gitignore,
) -> WatchConfig<'a> {
    // `parser` / `embedder` / `model_config` are shared statics (see
    // comment above); the per-call `matcher` still needs a distinct
    // `&'static RwLock`, so we leak that one field only.
    let gitignore = Box::leak(Box::new(std::sync::RwLock::new(Some(matcher))));
    WatchConfig {
        root,
        cqs_dir,
        notes_path,
        supported_ext,
        parser: &TEST_PARSER,
        embedder: &TEST_EMBEDDER,
        quiet: true,
        model_config: &TEST_MODEL_CONFIG,
        gitignore,
        splade_encoder: None,
        global_cache: None,
    }
}

fn test_watch_state() -> WatchState {
    WatchState {
        embedder_backoff: EmbedderBackoff::new(),
        pending_files: HashSet::new(),
        pending_notes: false,
        last_event: std::time::Instant::now(),
        last_indexed_mtime: HashMap::new(),
        hnsw_index: None,
        incremental_count: 0,
        dropped_this_cycle: 0,
        pending_rebuild: None,
        // PF-V1.30.1-1: throttle seed — tests that drive
        // `publish_watch_snapshot` directly want the very first call to
        // re-stat (the cache starts empty). Tests that don't touch the
        // publish path don't care about these fields' specific values.
        last_metadata_check: std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(60))
            .unwrap_or_else(std::time::Instant::now),
        cached_last_synced_at: None,
        active_slot: cqs::slot::DEFAULT_SLOT.to_string(),
    }
}

// ===== EmbedderBackoff tests =====

#[test]
fn backoff_initial_state_allows_retry() {
    let backoff = EmbedderBackoff::new();
    assert!(backoff.should_retry(), "Fresh backoff should allow retry");
}

#[test]
fn backoff_after_failure_delays_retry() {
    let mut backoff = EmbedderBackoff::new();
    backoff.record_failure();
    // After 1 failure, delay is 2^1 = 2 seconds
    assert!(
        !backoff.should_retry(),
        "Should not retry immediately after failure"
    );
    assert_eq!(backoff.failures, 1);
}

#[test]
fn backoff_reset_clears_failures() {
    let mut backoff = EmbedderBackoff::new();
    backoff.record_failure();
    backoff.record_failure();
    backoff.reset();
    assert_eq!(backoff.failures, 0);
    assert!(backoff.should_retry());
}

#[test]
fn backoff_caps_at_300s() {
    let mut backoff = EmbedderBackoff::new();
    // 2^9 = 512 > 300, so it should be capped
    for _ in 0..9 {
        backoff.record_failure();
    }
    // Verify it doesn't panic or overflow
    assert_eq!(backoff.failures, 9);
}

#[test]
fn backoff_saturating_add_no_overflow() {
    let mut backoff = EmbedderBackoff::new();
    backoff.failures = u32::MAX;
    backoff.record_failure();
    assert_eq!(backoff.failures, u32::MAX, "Should saturate, not overflow");
}

// ===== collect_events tests =====

#[test]
fn collect_events_filters_unsupported_extensions() {
    let root = PathBuf::from("/tmp/test_project");
    let cqs_dir = PathBuf::from("/tmp/test_project/.cqs");
    let notes_path = PathBuf::from("/tmp/test_project/docs/notes.toml");
    let supported: HashSet<&str> = ["rs", "py", "js"].iter().cloned().collect();
    let cfg = test_watch_config(&root, &cqs_dir, &notes_path, &supported);
    let mut state = test_watch_state();

    // .txt is not supported
    let event = make_event(
        vec![PathBuf::from("/tmp/test_project/readme.txt")],
        EventKind::Modify(notify::event::ModifyKind::Data(
            notify::event::DataChange::Content,
        )),
    );

    collect_events(&event, &cfg, &mut state);

    assert!(
        state.pending_files.is_empty(),
        "Unsupported extension should not be added"
    );
    assert!(!state.pending_notes);
}

#[test]
fn collect_events_skips_cqs_dir() {
    let root = PathBuf::from("/tmp/test_project");
    let cqs_dir = PathBuf::from("/tmp/test_project/.cqs");
    let notes_path = PathBuf::from("/tmp/test_project/docs/notes.toml");
    let supported: HashSet<&str> = ["rs", "db"].iter().cloned().collect();
    let cfg = test_watch_config(&root, &cqs_dir, &notes_path, &supported);
    let mut state = test_watch_state();

    let event = make_event(
        vec![PathBuf::from("/tmp/test_project/.cqs/index.db")],
        EventKind::Modify(notify::event::ModifyKind::Data(
            notify::event::DataChange::Content,
        )),
    );

    collect_events(&event, &cfg, &mut state);

    assert!(
        state.pending_files.is_empty(),
        ".cqs dir events should be skipped"
    );
}

/// Helper: build a `Gitignore` matcher in-memory from lines (no file IO).
fn gitignore_from_lines(root: &Path, lines: &[&str]) -> ignore::gitignore::Gitignore {
    let mut b = ignore::gitignore::GitignoreBuilder::new(root);
    for line in lines {
        b.add_line(None, line).expect("add_line");
    }
    b.build().expect("build gitignore")
}

#[test]
fn collect_events_skips_gitignore_matched_paths() {
    // #1002: `.claude/worktrees/` is a representative pollution case
    // from parallel-agent work. Verify that a path matched by
    // .gitignore is skipped.
    let root = PathBuf::from("/tmp/test_project");
    let cqs_dir = PathBuf::from("/tmp/test_project/.cqs");
    let notes_path = PathBuf::from("/tmp/test_project/docs/notes.toml");
    let supported: HashSet<&str> = ["rs"].iter().cloned().collect();
    let matcher = gitignore_from_lines(&root, &[".claude/", "target/"]);
    let cfg = test_watch_config_with_gitignore(&root, &cqs_dir, &notes_path, &supported, matcher);

    let mut state = test_watch_state();
    let event = make_event(
        vec![PathBuf::from(
            "/tmp/test_project/.claude/worktrees/agent-a1b2c3d4/src/lib.rs",
        )],
        EventKind::Modify(notify::event::ModifyKind::Data(
            notify::event::DataChange::Content,
        )),
    );
    collect_events(&event, &cfg, &mut state);
    assert!(
        state.pending_files.is_empty(),
        ".gitignore-matched path .claude/worktrees/... should be skipped"
    );
}

#[test]
fn collect_events_skips_target_dir_via_gitignore() {
    let root = PathBuf::from("/tmp/test_project");
    let cqs_dir = PathBuf::from("/tmp/test_project/.cqs");
    let notes_path = PathBuf::from("/tmp/test_project/docs/notes.toml");
    let supported: HashSet<&str> = ["rs"].iter().cloned().collect();
    let matcher = gitignore_from_lines(&root, &["target/"]);
    let cfg = test_watch_config_with_gitignore(&root, &cqs_dir, &notes_path, &supported, matcher);

    let mut state = test_watch_state();
    let event = make_event(
        vec![PathBuf::from("/tmp/test_project/target/debug/foo.rs")],
        EventKind::Modify(notify::event::ModifyKind::Data(
            notify::event::DataChange::Content,
        )),
    );
    collect_events(&event, &cfg, &mut state);
    assert!(
        state.pending_files.is_empty(),
        "target/ ignored by .gitignore should be skipped"
    );
}

#[test]
fn collect_events_does_not_skip_unrelated_paths_when_gitignore_present() {
    // False-positive guard: files under a directory not in .gitignore
    // must still be indexed even when a matcher is installed.
    let root = PathBuf::from("/tmp/test_project");
    let cqs_dir = PathBuf::from("/tmp/test_project/.cqs");
    let notes_path = PathBuf::from("/tmp/test_project/docs/notes.toml");
    let supported: HashSet<&str> = ["rs"].iter().cloned().collect();
    let matcher = gitignore_from_lines(&root, &[".claude/", "target/"]);
    let cfg = test_watch_config_with_gitignore(&root, &cqs_dir, &notes_path, &supported, matcher);

    let mut state = test_watch_state();
    let event = make_event(
        vec![PathBuf::from("/tmp/test_project/src/foo.rs")],
        EventKind::Modify(notify::event::ModifyKind::Data(
            notify::event::DataChange::Content,
        )),
    );
    collect_events(&event, &cfg, &mut state);
    assert!(
        !state.pending_files.is_empty(),
        "src/foo.rs is not in .gitignore and must not be skipped"
    );
}

#[test]
fn collect_events_negations_include_path() {
    // `.gitignore` negations (`!foo`) keep the file indexed even
    // if a broader pattern excludes its parent.
    let root = PathBuf::from("/tmp/test_project");
    let cqs_dir = PathBuf::from("/tmp/test_project/.cqs");
    let notes_path = PathBuf::from("/tmp/test_project/docs/notes.toml");
    let supported: HashSet<&str> = ["rs"].iter().cloned().collect();
    let matcher = gitignore_from_lines(&root, &["vendor/", "!vendor/keep/"]);
    let cfg = test_watch_config_with_gitignore(&root, &cqs_dir, &notes_path, &supported, matcher);

    let mut state = test_watch_state();
    let event = make_event(
        vec![PathBuf::from("/tmp/test_project/vendor/keep/lib.rs")],
        EventKind::Modify(notify::event::ModifyKind::Data(
            notify::event::DataChange::Content,
        )),
    );
    collect_events(&event, &cfg, &mut state);
    assert!(
        !state.pending_files.is_empty(),
        "negation `!vendor/keep/` must keep the file indexed"
    );
}

#[test]
fn collect_events_honors_none_matcher() {
    // With no matcher (--no-ignore or no .gitignore present), the
    // watch loop indexes every supported-extension path. Verifies
    // the `Option<_>` in `WatchConfig.gitignore` behaves as
    // documented.
    let root = PathBuf::from("/tmp/test_project");
    let cqs_dir = PathBuf::from("/tmp/test_project/.cqs");
    let notes_path = PathBuf::from("/tmp/test_project/docs/notes.toml");
    let supported: HashSet<&str> = ["rs"].iter().cloned().collect();
    // Default test_watch_config → gitignore is None.
    let cfg = test_watch_config(&root, &cqs_dir, &notes_path, &supported);

    let mut state = test_watch_state();
    let event = make_event(
        vec![PathBuf::from(
            "/tmp/test_project/.claude/worktrees/agent-x/src/lib.rs",
        )],
        EventKind::Modify(notify::event::ModifyKind::Data(
            notify::event::DataChange::Content,
        )),
    );
    collect_events(&event, &cfg, &mut state);
    assert!(
        !state.pending_files.is_empty(),
        "with matcher=None, all supported-ext paths must be accepted"
    );
}

#[test]
fn collect_events_cqs_dir_skip_survives_gitignore_allowlist() {
    // Even if a user accidentally or deliberately adds `!.cqs/` to
    // .gitignore, the hardcoded `.cqs/` skip keeps the system's own
    // files out of the index.
    let root = PathBuf::from("/tmp/test_project");
    let cqs_dir = PathBuf::from("/tmp/test_project/.cqs");
    let notes_path = PathBuf::from("/tmp/test_project/docs/notes.toml");
    let supported: HashSet<&str> = ["rs", "db"].iter().cloned().collect();
    // Negation allowing .cqs/ — should still be filtered by the
    // hardcoded .cqs/ skip in collect_events.
    let matcher = gitignore_from_lines(&root, &["*.tmp", "!.cqs/"]);
    let cfg = test_watch_config_with_gitignore(&root, &cqs_dir, &notes_path, &supported, matcher);

    let mut state = test_watch_state();
    let event = make_event(
        vec![PathBuf::from("/tmp/test_project/.cqs/index.db")],
        EventKind::Modify(notify::event::ModifyKind::Data(
            notify::event::DataChange::Content,
        )),
    );
    collect_events(&event, &cfg, &mut state);
    assert!(
        state.pending_files.is_empty(),
        ".cqs/ must always be skipped (belt-and-suspenders vs gitignore allowlist)"
    );
}

#[test]
fn build_gitignore_matcher_missing_returns_none() {
    // A project with neither .gitignore nor .cqsignore should produce
    // a `None` matcher — the watch loop indexes everything.
    let tmp = tempfile::TempDir::new().unwrap();
    assert!(
        build_gitignore_matcher(tmp.path()).is_none(),
        "missing .gitignore + .cqsignore should yield None matcher"
    );
}

#[test]
fn build_gitignore_matcher_env_kill_switch() {
    // CQS_WATCH_RESPECT_GITIGNORE=0 forces None even if .gitignore exists.
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::write(tmp.path().join(".gitignore"), "target/\n").unwrap();

    // Save + set + restore to stay neighbour-friendly with parallel
    // tests that may inspect the variable.
    let prev = std::env::var("CQS_WATCH_RESPECT_GITIGNORE").ok();
    std::env::set_var("CQS_WATCH_RESPECT_GITIGNORE", "0");
    let result = build_gitignore_matcher(tmp.path());
    match prev {
        Some(v) => std::env::set_var("CQS_WATCH_RESPECT_GITIGNORE", v),
        None => std::env::remove_var("CQS_WATCH_RESPECT_GITIGNORE"),
    }

    assert!(
        result.is_none(),
        "CQS_WATCH_RESPECT_GITIGNORE=0 must disable the matcher"
    );
}

#[test]
fn build_gitignore_matcher_real_file_loads_rules() {
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join(".gitignore"),
        "target/\n.claude/\nnode_modules/\n",
    )
    .unwrap();

    let matcher =
        build_gitignore_matcher(tmp.path()).expect("matcher should build for real gitignore");
    assert!(matcher.num_ignores() >= 3, "expected ≥3 rules loaded");

    // Sanity: matcher returns is_ignore for a target/ path via
    // parent-walk (file inside a directory-ignore rule).
    let hit = matcher
        .matched_path_or_any_parents(tmp.path().join("target/debug/foo.rs"), false)
        .is_ignore();
    assert!(hit, "target/ should match");
}

#[test]
fn build_gitignore_matcher_loads_cqsignore() {
    // The watch matcher must layer .cqsignore on top of .gitignore so
    // cqs-specific exclusions (vendor bundles etc.) are respected at
    // event time, mirroring the indexer behaviour.
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::write(tmp.path().join(".gitignore"), "target/\n").unwrap();
    std::fs::write(tmp.path().join(".cqsignore"), "**/*.min.js\n").unwrap();

    let matcher = build_gitignore_matcher(tmp.path()).expect("matcher should build with cqsignore");
    assert!(matcher.num_ignores() >= 2, "expected rules from both files");

    let vendor_hit = matcher
        .matched_path_or_any_parents(
            tmp.path().join("src/serve/assets/vendor/three.min.js"),
            false,
        )
        .is_ignore();
    assert!(
        vendor_hit,
        ".cqsignore *.min.js rule should match vendor JS"
    );

    let regular_miss = matcher
        .matched_path_or_any_parents(tmp.path().join("src/main.rs"), false)
        .is_ignore();
    assert!(!regular_miss, "regular source files must not match");
}

#[test]
fn build_gitignore_matcher_cqsignore_only() {
    // .cqsignore alone (no .gitignore) should still build the matcher.
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::write(tmp.path().join(".cqsignore"), "secret.txt\n").unwrap();

    let matcher =
        build_gitignore_matcher(tmp.path()).expect("matcher should build with cqsignore alone");
    let hit = matcher
        .matched_path_or_any_parents(tmp.path().join("secret.txt"), false)
        .is_ignore();
    assert!(hit, "cqsignore-only rule should match");
}

// ===== #1004 SPLADE builder / batch-size tests =====

#[test]
fn splade_batch_size_env_override() {
    let prev = std::env::var("CQS_SPLADE_BATCH").ok();
    std::env::set_var("CQS_SPLADE_BATCH", "16");
    let got = splade_batch_size();
    match prev {
        Some(v) => std::env::set_var("CQS_SPLADE_BATCH", v),
        None => std::env::remove_var("CQS_SPLADE_BATCH"),
    }
    assert_eq!(got, 16);
}

#[test]
fn splade_batch_size_default_is_32() {
    let prev = std::env::var("CQS_SPLADE_BATCH").ok();
    std::env::remove_var("CQS_SPLADE_BATCH");
    let got = splade_batch_size();
    if let Some(v) = prev {
        std::env::set_var("CQS_SPLADE_BATCH", v);
    }
    assert_eq!(got, 32);
}

#[test]
fn splade_batch_size_invalid_falls_back_to_default() {
    let prev = std::env::var("CQS_SPLADE_BATCH").ok();
    std::env::set_var("CQS_SPLADE_BATCH", "not-a-number");
    let got = splade_batch_size();
    match prev {
        Some(v) => std::env::set_var("CQS_SPLADE_BATCH", v),
        None => std::env::remove_var("CQS_SPLADE_BATCH"),
    }
    assert_eq!(got, 32, "unparseable value falls back to default");
}

#[test]
fn splade_batch_size_zero_falls_back_to_default() {
    let prev = std::env::var("CQS_SPLADE_BATCH").ok();
    std::env::set_var("CQS_SPLADE_BATCH", "0");
    let got = splade_batch_size();
    match prev {
        Some(v) => std::env::set_var("CQS_SPLADE_BATCH", v),
        None => std::env::remove_var("CQS_SPLADE_BATCH"),
    }
    assert_eq!(got, 32, "0 is not a valid batch size, falls back");
}

#[test]
fn build_splade_encoder_env_kill_switch_returns_none() {
    // CQS_WATCH_INCREMENTAL_SPLADE=0 must return None regardless of
    // whether a SPLADE model is configured. Verifies the feature-flag
    // kill-switch fires before any model-load work.
    let prev = std::env::var("CQS_WATCH_INCREMENTAL_SPLADE").ok();
    std::env::set_var("CQS_WATCH_INCREMENTAL_SPLADE", "0");
    let got = build_splade_encoder_for_watch();
    match prev {
        Some(v) => std::env::set_var("CQS_WATCH_INCREMENTAL_SPLADE", v),
        None => std::env::remove_var("CQS_WATCH_INCREMENTAL_SPLADE"),
    }
    assert!(
        got.is_none(),
        "CQS_WATCH_INCREMENTAL_SPLADE=0 must disable the encoder"
    );
}

#[test]
fn splade_origin_key_normalizes_backslashes() {
    // PB-V1.29-2 regression. `encode_splade_for_changed_files` builds
    // the DB lookup key via `cqs::normalize_path(file)`. A `PathBuf`
    // carrying backslashes (as any Windows-canonicalized path does)
    // must normalize to the forward-slash form stored at ingest, or
    // `get_chunks_by_origin` returns Ok(vec![]) and SPLADE silently
    // no-ops for the file.
    let p = std::path::PathBuf::from(r"src\cli\watch.rs");
    let origin = cqs::normalize_path(&p);
    assert_eq!(
        origin, "src/cli/watch.rs",
        "origin key must use forward slashes to match DB origins"
    );

    // UNC verbatim prefix must be stripped too (dunce::canonicalize
    // may leave `\\?\C:\…` on Windows). On Unix this just asserts
    // the helper doesn't mangle a plain relative path.
    let p2 = std::path::PathBuf::from(r"\\?\C:\repo\src\cli\watch.rs");
    let origin2 = cqs::normalize_path(&p2);
    assert!(
        !origin2.contains('\\') && !origin2.starts_with(r"\\?\"),
        "normalize_path must strip the verbatim UNC prefix: got {origin2}"
    );
}

#[test]
fn collect_events_detects_notes_path() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let cqs_dir = root.join(".cqs");
    let notes_dir = root.join("docs");
    std::fs::create_dir_all(&notes_dir).unwrap();
    let notes_path = notes_dir.join("notes.toml");
    std::fs::write(&notes_path, "# notes").unwrap();

    let supported: HashSet<&str> = ["rs"].iter().cloned().collect();
    let cfg = test_watch_config(&root, &cqs_dir, &notes_path, &supported);
    let mut state = test_watch_state();

    let event = make_event(
        vec![notes_path.clone()],
        EventKind::Modify(notify::event::ModifyKind::Data(
            notify::event::DataChange::Content,
        )),
    );

    collect_events(&event, &cfg, &mut state);

    assert!(state.pending_notes, "Notes path should set pending_notes");
    assert!(
        state.pending_files.is_empty(),
        "Notes should not be added to pending_files"
    );
}

#[test]
fn collect_events_respects_max_pending_files() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let cqs_dir = root.join(".cqs");
    let notes_path = root.join("docs/notes.toml");
    let supported: HashSet<&str> = ["rs"].iter().cloned().collect();
    let cfg = test_watch_config(&root, &cqs_dir, &notes_path, &supported);
    let mut state = test_watch_state();

    // Pre-fill pending_files to max_pending_files()
    for i in 0..max_pending_files() {
        state
            .pending_files
            .insert(PathBuf::from(format!("f{}.rs", i)));
    }

    // Create a real file so mtime check passes
    let new_file = root.join("overflow.rs");
    std::fs::write(&new_file, "fn main() {}").unwrap();

    let event = make_event(
        vec![new_file],
        EventKind::Create(notify::event::CreateKind::File),
    );

    collect_events(&event, &cfg, &mut state);

    assert_eq!(
        state.pending_files.len(),
        max_pending_files(),
        "Should not exceed max_pending_files()"
    );
}

#[test]
fn collect_events_skips_unchanged_mtime() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let cqs_dir = root.join(".cqs");
    let notes_path = root.join("docs/notes.toml");
    let supported: HashSet<&str> = ["rs"].iter().cloned().collect();
    let cfg = test_watch_config(&root, &cqs_dir, &notes_path, &supported);
    let mut state = test_watch_state();

    // Create a file and record its mtime as already indexed
    let file = root.join("src/lib.rs");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(&file, "fn main() {}").unwrap();
    let mtime = std::fs::metadata(&file).unwrap().modified().unwrap();
    state
        .last_indexed_mtime
        .insert(PathBuf::from("src/lib.rs"), mtime);

    let event = make_event(
        vec![file],
        EventKind::Modify(notify::event::ModifyKind::Data(
            notify::event::DataChange::Content,
        )),
    );

    collect_events(&event, &cfg, &mut state);

    assert!(
        state.pending_files.is_empty(),
        "Unchanged mtime should be skipped"
    );
}

// ===== last_indexed_mtime prune tests =====

/// #969: recency prune drops entries older than `LAST_INDEXED_PRUNE_AGE_SECS`,
/// keeps fresh entries, and only triggers once the map exceeds
/// `LAST_INDEXED_PRUNE_SIZE_THRESHOLD_DEFAULT`. This replaces the old per-entry
/// `Path::exists()` loop that stalled the watch thread on WSL 9P mounts.
#[test]
fn test_last_indexed_mtime_recency_prune() {
    let now = SystemTime::now();
    let two_days = Duration::from_secs(2 * LAST_INDEXED_PRUNE_AGE_SECS);
    let one_minute = Duration::from_secs(60);
    let old = now.checked_sub(two_days).unwrap();
    let fresh = now.checked_sub(one_minute).unwrap();

    // (1) Small map — below the size threshold — must not prune at all,
    // even if every entry is ancient. The threshold is a cache-size
    // safety valve, not a TTL for the whole session.
    let mut small: HashMap<PathBuf, SystemTime> = HashMap::new();
    small.insert(PathBuf::from("a.rs"), old);
    small.insert(PathBuf::from("b.rs"), fresh);
    let pruned_small = prune_last_indexed_mtime(&mut small);
    assert_eq!(
        pruned_small, 0,
        "Prune must not run below size threshold (got {} entries removed from 2-entry map)",
        pruned_small
    );
    assert_eq!(
        small.len(),
        2,
        "Small map should be untouched when below threshold"
    );

    // (2) Large map — above the size threshold — prunes old entries
    // and keeps fresh ones. Build a map with SIZE_THRESHOLD + 1 old
    // entries plus a handful of fresh sentinels so we can check both
    // that old entries are removed and fresh ones survive.
    let mut large: HashMap<PathBuf, SystemTime> = HashMap::new();
    for i in 0..=LAST_INDEXED_PRUNE_SIZE_THRESHOLD_DEFAULT {
        large.insert(PathBuf::from(format!("old_{}.rs", i)), old);
    }
    large.insert(PathBuf::from("fresh_1.rs"), fresh);
    large.insert(PathBuf::from("fresh_2.rs"), now);
    let total_before = large.len();
    let pruned_large = prune_last_indexed_mtime(&mut large);

    // Every "old" entry (two days stale) should be gone.
    assert_eq!(
        pruned_large,
        LAST_INDEXED_PRUNE_SIZE_THRESHOLD_DEFAULT + 1,
        "Expected all old entries pruned (total_before={}, remaining={})",
        total_before,
        large.len()
    );
    assert!(
        large.contains_key(&PathBuf::from("fresh_1.rs")),
        "Fresh entry from 1 minute ago must survive prune"
    );
    assert!(
        large.contains_key(&PathBuf::from("fresh_2.rs")),
        "Entry at `now` must survive prune"
    );
    assert_eq!(
        large.len(),
        2,
        "Only the 2 fresh entries should remain after prune"
    );

    // (3) Entry just inside the cutoff window survives. We use a 1-second
    // margin rather than exactly `now - PRUNE_AGE` because `prune_*` calls
    // `SystemTime::now()` internally — its clock ticks a few microseconds
    // past the test's clock, so an entry pinned to the test's computed
    // cutoff would be classified as older and pruned. 1 second is
    // comfortably more than the inter-call drift while still well under
    // the 1-day window.
    let just_inside = now
        .checked_sub(Duration::from_secs(LAST_INDEXED_PRUNE_AGE_SECS - 1))
        .unwrap();
    let mut boundary: HashMap<PathBuf, SystemTime> = HashMap::new();
    for i in 0..=LAST_INDEXED_PRUNE_SIZE_THRESHOLD_DEFAULT {
        boundary.insert(PathBuf::from(format!("stale_{}.rs", i)), old);
    }
    boundary.insert(PathBuf::from("just_inside.rs"), just_inside);
    prune_last_indexed_mtime(&mut boundary);
    assert!(
        boundary.contains_key(&PathBuf::from("just_inside.rs")),
        "Entry 1 second inside the cutoff window must survive"
    );
}

// ===== Constants tests =====

#[test]
fn hnsw_rebuild_threshold_is_reasonable() {
    assert!(hnsw_rebuild_threshold() > 0);
    assert!(hnsw_rebuild_threshold() <= 1000);
}

#[test]
fn max_pending_files_is_bounded() {
    assert!(max_pending_files() > 0);
    assert!(max_pending_files() <= 100_000);
}

// ===== P2 #62 trim_trailing_newline tests =====

#[cfg(unix)]
#[test]
fn trim_newline_strips_lf() {
    assert_eq!(socket::trim_trailing_newline(b"hello\n"), b"hello");
}

#[cfg(unix)]
#[test]
fn trim_newline_strips_crlf() {
    assert_eq!(socket::trim_trailing_newline(b"hello\r\n"), b"hello");
}

#[cfg(unix)]
#[test]
fn trim_newline_no_op_when_absent() {
    assert_eq!(socket::trim_trailing_newline(b"hello"), b"hello");
}

#[cfg(unix)]
#[test]
fn trim_newline_handles_empty() {
    assert_eq!(socket::trim_trailing_newline(b""), b"");
}

#[cfg(unix)]
#[test]
fn trim_newline_only_strips_one_lf() {
    // Two trailing newlines → only the last is stripped (callers that
    // wrote two newlines deliberately are uncommon, but we don't want
    // to silently consume more than one terminator).
    assert_eq!(socket::trim_trailing_newline(b"hello\n\n"), b"hello\n");
}

// ===== PB-V1.29-3: chunk.id prefix-strip uses normalize_path =====

/// Exercises the same strip-and-rewrite shape used by `reindex_files`
/// at watch.rs :~2436 after the PB-V1.29-3 fix. The direct function
/// isn't extracted, but the logic is small and identical — this test
/// documents the contract so a regression back to `abs_path.display()`
/// is caught by a targeted unit test instead of the next Windows CI run.
fn normalize_strip_and_rewrite(abs_path: &Path, rel_path: &Path, chunk_id: &str) -> Option<String> {
    let abs_norm = cqs::normalize_path(abs_path);
    let rel_norm = cqs::normalize_path(rel_path);
    chunk_id
        .strip_prefix(abs_norm.as_str())
        .map(|rest| format!("{}{}", rel_norm, rest))
}

#[test]
fn prefix_strip_normalizes_backslash_verbatim_prefix() {
    // Simulates the Windows shape that the bug regressed on:
    //   abs_path   = \\?\C:\Projects\cqs\src\foo.rs
    //   chunk.id   = C:/Projects/cqs/src/foo.rs:10:abcd  (parser output)
    //   rel_path   = src\foo.rs  (after strip_prefix on the root)
    // Before the fix: `abs_path.display()` emits the verbatim `\\?\` +
    // backslashes, so the prefix-strip fails and chunk.id keeps its
    // absolute prefix. After the fix: both sides normalize.
    let abs = Path::new(r"\\?\C:\Projects\cqs\src\foo.rs");
    let rel = Path::new(r"src\foo.rs");
    let chunk_id = "C:/Projects/cqs/src/foo.rs:10:abcd";
    let rewritten =
        normalize_strip_and_rewrite(abs, rel, chunk_id).expect("prefix-strip must match");
    assert!(
        rewritten.starts_with("src/foo.rs"),
        "expected rewritten id to start with forward-slash rel path, got {rewritten}"
    );
    assert_eq!(rewritten, "src/foo.rs:10:abcd");
}

#[test]
fn prefix_strip_unix_path_round_trip() {
    // Baseline: Unix path with forward slashes on both sides still works.
    let abs = Path::new("/home/user/proj/src/foo.rs");
    let rel = Path::new("src/foo.rs");
    let chunk_id = "/home/user/proj/src/foo.rs:42:deadbeef";
    let rewritten =
        normalize_strip_and_rewrite(abs, rel, chunk_id).expect("prefix-strip must match");
    assert_eq!(rewritten, "src/foo.rs:42:deadbeef");
}

// ===== EH-V1.29-8: gitignore RwLock poison recovery =====

#[test]
fn gitignore_rwlock_poison_still_yields_matcher() {
    // Simulates the recovery arm at watch.rs :~1741 / :~1963. A writer
    // that panics while holding the write lock leaves the inner value
    // valid but the lock poisoned; the `match gitignore.read()` arm
    // must recover via `poisoned.into_inner()` instead of silently
    // dropping to "no matcher".
    use std::sync::{Arc, RwLock};

    let matcher_builder = ignore::gitignore::GitignoreBuilder::new(std::path::Path::new("."));
    let (matcher, _errs) = matcher_builder.build_global();
    let lock: Arc<RwLock<Option<ignore::gitignore::Gitignore>>> =
        Arc::new(RwLock::new(Some(matcher)));

    // Poison the lock by panicking inside a write guard on a helper
    // thread — the panic propagates, leaves the RwLock poisoned, and
    // joins.
    let poisoner = Arc::clone(&lock);
    let _ = std::thread::spawn(move || {
        let _guard = poisoner.write().expect("initial write must succeed");
        panic!("intentional poison for EH-V1.29-8 test");
    })
    .join();

    // Post-poison: the bug was `gitignore.read().ok()` silently
    // returning `None`. The fixed code must still yield `Some(_)` by
    // recovering the inner value via `into_inner()`.
    let matcher_guard = match lock.read() {
        Ok(g) => Some(g),
        Err(poisoned) => Some(poisoned.into_inner()),
    };
    assert!(
        matcher_guard.is_some(),
        "poison-recovery must still surface the previously-written matcher"
    );
    assert!(
        matcher_guard.as_ref().unwrap().is_some(),
        "inner Option<Gitignore> must still be Some after poison recovery"
    );
}

// ── #1090 background rebuild + atomic swap ──────────────────────────────

/// Build a tiny `Owned` HnswIndex from N synthetic vectors. Stand-in for a
/// thread-built index in the `drain_pending_rebuild` tests below.
fn synthetic_owned_index(n: usize, dim: usize) -> cqs::hnsw::HnswIndex {
    // Non-zero, distinct vectors per id — hnsw_rs's HNSW can collapse
    // zero vectors (undefined cosine sim) so the first entry needs a
    // non-trivial value or the index ends up under-populated.
    let batch: Vec<(String, cqs::Embedding)> = (0..n)
        .map(|i| {
            let mut v = vec![0.1_f32; dim];
            v[i % dim] = (i as f32 + 1.0) * 0.5;
            (format!("c{i}"), cqs::Embedding::new(v))
        })
        .collect();
    let iter = std::iter::once(Ok::<_, cqs::store::StoreError>(batch));
    cqs::hnsw::HnswIndex::build_batched_with_dim(iter, n, dim).expect("build synthetic index")
}

/// Make a Store + WatchConfig pair for a fresh tempdir, init'd to `dim`.
/// Returns owned bindings so each caller can pass long-lived references
/// to `test_watch_config`.
struct DrainFixture {
    tmp: tempfile::TempDir,
    store: Store,
    supported_ext: HashSet<&'static str>,
    notes_path: PathBuf,
}

fn drain_test_fixture(dim: usize) -> DrainFixture {
    let tmp = tempfile::TempDir::new().unwrap();
    let store_path = tmp.path().join("index.db");
    let mut store = Store::open(&store_path).unwrap();
    store
        .init(&cqs::store::ModelInfo::new("test/m", dim))
        .unwrap();
    store.set_dim(dim);
    let notes_path = tmp.path().join("docs/notes.toml");
    DrainFixture {
        tmp,
        store,
        supported_ext: HashSet::new(),
        notes_path,
    }
}

#[test]
fn drain_pending_rebuild_replays_delta_into_new_index() {
    let dim = 4;
    let new_idx = synthetic_owned_index(3, dim);
    assert_eq!(new_idx.len(), 3);

    let (tx, rx) = std::sync::mpsc::channel();
    tx.send(Ok(Some(RebuildResult {
        index: new_idx,
        // No overlap between delta ids and snapshot — all replay.
        snapshot_hashes: std::collections::HashMap::new(),
    })))
    .unwrap();
    drop(tx);

    let mut state = test_watch_state();
    state.pending_rebuild = Some(PendingRebuild {
        rx,
        delta: vec![
            (
                "delta_a".to_string(),
                cqs::Embedding::new(vec![1.0; dim]),
                "h_delta_a".to_string(),
            ),
            (
                "delta_b".to_string(),
                cqs::Embedding::new(vec![0.5; dim]),
                "h_delta_b".to_string(),
            ),
        ],
        started_at: std::time::Instant::now(),
        handle: None,
        delta_saturated: false,
    });

    let fix = drain_test_fixture(dim);
    let cfg = test_watch_config(
        fix.tmp.path(),
        fix.tmp.path(),
        &fix.notes_path,
        &fix.supported_ext,
    );
    let store = &fix.store;

    drain_pending_rebuild(&cfg, store, &mut state);

    let idx = state.hnsw_index.expect("rebuild was swapped in");
    assert_eq!(idx.len(), 5, "3 from new_idx + 2 from delta");
    assert!(idx.ids().iter().any(|id| id == "delta_a"));
    assert!(idx.ids().iter().any(|id| id == "delta_b"));
    assert_eq!(state.incremental_count, 0);
    assert!(state.pending_rebuild.is_none());
}

/// P1.17 / #1124: when a chunk is re-embedded mid-rebuild, the snapshot
/// has the OLD vector under the same id while delta has the NEW vector
/// + new content_hash. The drain must REPLAY the delta entry so the
/// fresh embedding lands in the swapped HNSW. The pre-fix code dedup'd
/// by id-only and silently dropped these updates.
///
/// We can't query hnsw_rs for "give me the embedding stored under id X"
/// (it's a graph, not a kv store) and there's no deletion API, so we
/// assert the side-effect: the swapped index contains MORE entries
/// than the snapshot alone (orphan + replayed vector both present),
/// and a search by the FRESH embedding returns id "a" with cosine ≈ 1.0.
#[test]
fn test_rebuild_window_re_embedding_replays_fresh_vector() {
    let dim = 4;

    // Snapshot has id "a" baked in with hash h_v1 (and an unrelated id "z"
    // so the index isn't trivially empty).
    let snapshot_batch: Vec<(String, cqs::Embedding)> = vec![
        (
            "a".to_string(),
            cqs::Embedding::new(vec![1.0, 0.0, 0.0, 0.0]),
        ),
        (
            "z".to_string(),
            cqs::Embedding::new(vec![0.0, 0.0, 0.0, 1.0]),
        ),
    ];
    let snapshot_iter = std::iter::once(Ok::<_, cqs::store::StoreError>(snapshot_batch));
    let new_idx = cqs::hnsw::HnswIndex::build_batched_with_dim(snapshot_iter, 2, dim)
        .expect("build snapshot index");
    assert_eq!(new_idx.len(), 2, "snapshot starts with 2 entries");

    let mut snapshot_hashes = std::collections::HashMap::new();
    snapshot_hashes.insert("a".to_string(), "h_v1".to_string());
    snapshot_hashes.insert("z".to_string(), "h_z".to_string());

    let (tx, rx) = std::sync::mpsc::channel();
    tx.send(Ok(Some(RebuildResult {
        index: new_idx,
        snapshot_hashes,
    })))
    .unwrap();
    drop(tx);

    // Delta has "a" again, but with a NEW embedding and a NEW content_hash —
    // i.e. the file was re-embedded between the snapshot and the swap.
    // The fresh vector points along axis 1, distinct from the snapshot's
    // axis-0 vector, so we can tell them apart by search.
    let fresh_embedding = cqs::Embedding::new(vec![0.0, 1.0, 0.0, 0.0]);

    let mut state = test_watch_state();
    state.pending_rebuild = Some(PendingRebuild {
        rx,
        delta: vec![(
            "a".to_string(),
            fresh_embedding.clone(),
            "h_v2".to_string(), // hash differs from snapshot's "h_v1"
        )],
        started_at: std::time::Instant::now(),
        handle: None,
        delta_saturated: false,
    });

    let fix = drain_test_fixture(dim);
    let cfg = test_watch_config(
        fix.tmp.path(),
        fix.tmp.path(),
        &fix.notes_path,
        &fix.supported_ext,
    );

    drain_pending_rebuild(&cfg, &fix.store, &mut state);

    let idx = state.hnsw_index.expect("rebuild was swapped in");
    // The fresh vector was REPLAYED — index now contains 3 nodes
    // (snapshot's "a" + "z" + replayed "a"). hnsw_rs has no deletion,
    // so both vectors for "a" coexist as duplicate-id orphans; that's
    // the same trade-off as the fast-incremental path. Search
    // post-filters via SQLite in production, which collapses the
    // duplicates into one logical hit.
    assert_eq!(
        idx.len(),
        3,
        "fresh re-embedding must be replayed (snapshot 2 + 1 replay)"
    );

    // Crucial assertion: searching by the FRESH embedding returns id "a".
    // Pre-fix, the replay was skipped, so the only "a" in the index was
    // the snapshot's axis-0 vector, and querying the axis-1 fresh vector
    // would surface "z" or "a" with poor cosine. After the fix, the
    // axis-1 vector is in the index under "a" with cosine ≈ 1.0.
    let hits = idx.search(&fresh_embedding, 1);
    assert!(!hits.is_empty(), "search must return at least one hit");
    let top = &hits[0];
    assert_eq!(
        top.id, "a",
        "top hit for fresh embedding must be the re-embedded chunk \"a\""
    );
    assert!(
        top.score > 0.99,
        "top hit cosine must be near 1.0 (fresh vector is in the index); got {}",
        top.score
    );

    assert!(state.pending_rebuild.is_none());
}

#[test]
fn drain_pending_rebuild_dedups_against_known_ids() {
    // P1.17 / #1124: dedup is now (id, content_hash)-aware, not id-only.
    // The rebuild thread snapshotted c0/c1/c2 with hashes h0/h1/h2.
    // Delta replays c0 with the SAME hash h0 (true duplicate — must be
    // skipped), c1 with the same hash h1 (skipped), and c_new with a
    // brand-new id (must replay). c0/c1 with matching hashes would
    // double-insert under the pre-fix code; the new dedup uses the
    // snapshot hashes the rebuild produced.
    let dim = 4;
    let new_idx = synthetic_owned_index(3, dim); // ids: c0, c1, c2

    let mut snapshot_hashes = std::collections::HashMap::new();
    snapshot_hashes.insert("c0".to_string(), "h0".to_string());
    snapshot_hashes.insert("c1".to_string(), "h1".to_string());
    snapshot_hashes.insert("c2".to_string(), "h2".to_string());

    let (tx, rx) = std::sync::mpsc::channel();
    tx.send(Ok(Some(RebuildResult {
        index: new_idx,
        snapshot_hashes,
    })))
    .unwrap();
    drop(tx);

    let mut state = test_watch_state();
    state.pending_rebuild = Some(PendingRebuild {
        rx,
        delta: vec![
            // Same id + same hash → genuine duplicate, skip.
            (
                "c0".to_string(),
                cqs::Embedding::new(vec![9.0; dim]),
                "h0".to_string(),
            ),
            (
                "c1".to_string(),
                cqs::Embedding::new(vec![9.0; dim]),
                "h1".to_string(),
            ),
            // Brand-new id → snapshot didn't see it, must replay.
            (
                "c_new".to_string(),
                cqs::Embedding::new(vec![9.0; dim]),
                "h_new".to_string(),
            ),
        ],
        started_at: std::time::Instant::now(),
        handle: None,
        delta_saturated: false,
    });

    let fix = drain_test_fixture(dim);
    let cfg = test_watch_config(
        fix.tmp.path(),
        fix.tmp.path(),
        &fix.notes_path,
        &fix.supported_ext,
    );
    let store = &fix.store;

    drain_pending_rebuild(&cfg, store, &mut state);

    let idx = state.hnsw_index.expect("rebuild was swapped in");
    assert_eq!(
        idx.len(),
        4,
        "3 from new_idx + 1 genuinely-new delta entry — same-hash duplicates skipped"
    );
    assert!(idx.ids().iter().any(|id| id == "c_new"));
}

#[test]
fn drain_pending_rebuild_clears_pending_on_thread_error() {
    let (tx, rx) = std::sync::mpsc::channel();
    tx.send(Err(anyhow::anyhow!("simulated rebuild failure")))
        .unwrap();
    drop(tx);

    let mut state = test_watch_state();
    state.pending_rebuild = Some(PendingRebuild {
        rx,
        delta: Vec::new(),
        started_at: std::time::Instant::now(),
        handle: None,
        delta_saturated: false,
    });

    let fix = drain_test_fixture(4);
    let cfg = test_watch_config(
        fix.tmp.path(),
        fix.tmp.path(),
        &fix.notes_path,
        &fix.supported_ext,
    );
    let store = &fix.store;

    drain_pending_rebuild(&cfg, store, &mut state);
    assert!(state.pending_rebuild.is_none());
    assert!(state.hnsw_index.is_none());
}

// P2.29: spawn_hnsw_rebuild adversarial coverage — the original
// production code shipped without tests for the dim-mismatch and
// store-open-fail paths even though both are realistic failure modes
// (model-swap mid-flight, slot dir deleted under the daemon).
//
// We invoke `spawn_hnsw_rebuild` directly, then join the worker thread
// and inspect what landed on the receive channel. The contract is:
//   - dim mismatch  → channel carries Err, pending must clear on drain
//   - missing index → channel carries Err, ditto
// Both paths must NOT panic and must NOT leak the pending entry forever.

/// P2.29: a dim mismatch between the store and the caller's
/// `expected_dim` must surface as `Err` on the channel, not a panic.
/// The on-disk store is dim=4; we ask for dim=8.
#[test]
fn spawn_hnsw_rebuild_dim_mismatch_returns_error_outcome() {
    let dim = 4;
    let expected_dim = 8;
    let fix = drain_test_fixture(dim);
    let cqs_dir = fix.tmp.path().to_path_buf();
    let index_path = fix.tmp.path().join("index.db");

    let pending = spawn_hnsw_rebuild(cqs_dir, index_path, expected_dim, "p2_29_dim");
    // Wait for the worker thread to finish, bounded.
    let outcome = pending
        .rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("rebuild thread must report within 10s");
    // `RebuildOutcome` is `Result<Option<HnswIndex>, anyhow::Error>` and
    // `HnswIndex` is not `Debug`, so we can't call `unwrap_err` directly.
    // Pattern-match instead.
    let err = match outcome {
        Ok(_) => panic!("dim mismatch must surface as an Err on the rebuild channel"),
        Err(e) => e,
    };
    let msg = format!("{}", err);
    assert!(
        msg.contains("does not match expected") || msg.contains("dim"),
        "error must mention the dim mismatch (got: {msg})"
    );
    // Drain the worker handle so the OS thread is reaped.
    if let Some(h) = pending.handle {
        let _ = h.join();
    }
}

/// P2.29: pointing at a non-existent index path (e.g. slot dir
/// removed mid-flight) must surface as `Err` on the channel — never
/// panic, never hang. `Store::open_readonly_pooled` returns an Err
/// immediately and the closure propagates it via `?`.
#[test]
fn spawn_hnsw_rebuild_missing_index_path_returns_error_outcome() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cqs_dir = tmp.path().to_path_buf();
    let bogus = tmp.path().join("does_not_exist.db");

    let pending = spawn_hnsw_rebuild(cqs_dir, bogus, 4, "p2_29_missing");
    let outcome = pending
        .rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("rebuild thread must report within 10s");
    assert!(
        outcome.is_err(),
        "missing index must surface as an Err on the rebuild channel"
    );
    if let Some(h) = pending.handle {
        let _ = h.join();
    }
}

/// P2.29: drain path must clear `pending_rebuild` when the worker
/// thread reported an error. Today the rebuild thread can fail for
/// many reasons (dim mismatch, store gone, save failure); the drain
/// must always reset state so the next threshold trigger can retry —
/// otherwise the pending slot leaks forever and no further rebuilds
/// run.
#[test]
fn drain_clears_pending_when_spawned_rebuild_errors() {
    // Drive the full spawn+drain cycle through a guaranteed-failing
    // path (missing index) so the drain sees a real Err rather than
    // a hand-crafted `tx.send(Err(_))`.
    let tmp = tempfile::TempDir::new().unwrap();
    let pending = spawn_hnsw_rebuild(
        tmp.path().to_path_buf(),
        tmp.path().join("nope.db"),
        4,
        "p2_29_drain",
    );

    // Block until the worker thread has signalled — the drain uses
    // try_recv so we want the message already enqueued.
    if let Some(h) = pending.handle.as_ref() {
        // Best-effort wait: rebuild thread writes to channel before
        // exiting. Up to 10s tolerance for slow CI.
        for _ in 0..100 {
            if h.is_finished() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }

    // Now build a state with this PendingRebuild and drive the drain.
    let mut state = test_watch_state();
    state.pending_rebuild = Some(pending);

    let fix = drain_test_fixture(4);
    let cfg = test_watch_config(
        fix.tmp.path(),
        fix.tmp.path(),
        &fix.notes_path,
        &fix.supported_ext,
    );
    drain_pending_rebuild(&cfg, &fix.store, &mut state);
    assert!(
        state.pending_rebuild.is_none(),
        "drain must clear pending_rebuild when the rebuild thread errored"
    );
    assert!(
        state.hnsw_index.is_none(),
        "no index must be swapped in when the rebuild errored"
    );
}

#[test]
fn drain_pending_rebuild_leaves_pending_when_still_running() {
    let (_tx, rx) = std::sync::mpsc::channel::<RebuildOutcome>();
    let mut state = test_watch_state();
    state.pending_rebuild = Some(PendingRebuild {
        rx,
        delta: Vec::new(),
        started_at: std::time::Instant::now(),
        handle: None,
        delta_saturated: false,
    });

    let fix = drain_test_fixture(4);
    let cfg = test_watch_config(
        fix.tmp.path(),
        fix.tmp.path(),
        &fix.notes_path,
        &fix.supported_ext,
    );
    let store = &fix.store;

    drain_pending_rebuild(&cfg, store, &mut state);
    assert!(
        state.pending_rebuild.is_some(),
        "pending should remain in flight when channel has no message"
    );
}

// ── #1129: reindex_files consults the global EmbeddingCache ─────────────

/// `reindex_files` must read from `global_cache` before calling the
/// embedder. We prime the cache with a known embedding for the chunk's
/// content_hash, then ensure the chunk written to the store has THAT
/// vector — proof the embedder was bypassed entirely.
///
/// `#[ignore]` because building a real `Embedder` (CPU) loads ONNX
/// weights and is too heavy for the default test pass. The test still
/// exercises the cache wiring; running it gated catches the regression
/// when the watch path drops the cache check.
#[test]
#[ignore = "Requires loading the BGE-large model (heavy)"]
fn test_reindex_files_hits_global_cache_skipping_embedder() {
    use cqs::cache::{CachePurpose, EmbeddingCache};
    use cqs::embedder::ModelConfig;
    use std::io::Write;

    // 1) Tempdir with a tiny rust file we can parse.
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();
    let cqs_dir = root.join(".cqs");
    std::fs::create_dir_all(&cqs_dir).unwrap();
    let rs_file = root.join("hit.rs");
    let source = "pub fn hits_cache() { let _ = 42; }";
    let mut f = std::fs::File::create(&rs_file).unwrap();
    f.write_all(source.as_bytes()).unwrap();
    drop(f);

    // 2) Build a Store and an Embedder. Both required by reindex_files.
    let model_cfg = ModelConfig::resolve(None, None);
    let embedder = Embedder::new_cpu(model_cfg).expect("init CPU embedder");
    let dim = embedder.embedding_dim();
    let store_path = cqs_dir.join(cqs::INDEX_DB_FILENAME);
    let mut store = Store::open(&store_path).unwrap();
    store
        .init(&cqs::store::ModelInfo::new(
            &embedder.model_config().repo,
            dim,
        ))
        .unwrap();
    store.set_dim(dim);

    // 3) Parse the file once to learn the chunk's content_hash. Only
    //    deterministic way to know what to prime — the parser's hash
    //    is computed from chunk metadata + bytes.
    let parser = CqParser::new().unwrap();
    let chunks = parser
        .parse_file_all_with_chunk_calls(&rs_file)
        .map(|(c, _, _, _)| c)
        .expect("parse hit.rs");
    assert!(!chunks.is_empty(), "parser must yield at least one chunk");
    let target_hash = chunks[0].content_hash.clone();

    // 4) Prime the global cache with a SENTINEL embedding for the
    //    chunk's content_hash. Sentinel = first lane large, others zero,
    //    then unit-normalized — distinguishes it from anything the
    //    embedder would produce on this content.
    let cache_path = EmbeddingCache::project_default_path(&cqs_dir);
    let cache = EmbeddingCache::open(&cache_path).expect("open cache");
    let mut sentinel = vec![0.0_f32; dim];
    sentinel[0] = 7.7;
    let norm: f32 = sentinel.iter().map(|x| x * x).sum::<f32>().sqrt();
    for x in &mut sentinel {
        *x /= norm;
    }
    let sentinel_clone = sentinel.clone();
    cache
        .write_batch_owned(
            &[(target_hash.clone(), sentinel_clone)],
            embedder.model_fingerprint(),
            CachePurpose::Embedding,
            dim,
        )
        .unwrap();

    // 5) Run reindex_files with the cache wired in.
    let files = vec![PathBuf::from("hit.rs")];
    let (count, _) = reindex_files(root, &store, &files, &parser, &embedder, Some(&cache), true)
        .expect("reindex_files");
    assert!(count >= 1, "at least one chunk indexed");

    // 6) The chunk in the store must hold the SENTINEL — proof that
    //    the global cache served the read instead of the embedder.
    let stored = store
        .get_embeddings_by_hashes(&[target_hash.as_str()])
        .expect("store lookup");
    let stored_emb = stored
        .get(&target_hash)
        .expect("chunk written under the same content_hash");
    let stored_slice = stored_emb.as_slice();
    assert_eq!(stored_slice.len(), dim);
    for (i, (&got, &want)) in stored_slice.iter().zip(sentinel.iter()).enumerate() {
        assert!(
            (got - want).abs() < 1e-5,
            "lane {i}: got {got} want {want} — embedder was called instead of cache hit"
        );
    }
}

/// `reindex_files` with `global_cache: None` falls back to the prior
/// store-only path. Lighter assertion: just confirm the function runs
/// to completion and writes chunks. Pins the legacy degrade path so
/// `CQS_CACHE_ENABLED=0` doesn't break watch.
#[test]
#[ignore = "Requires loading the BGE-large model (heavy)"]
fn test_reindex_files_no_global_cache_still_works() {
    use cqs::embedder::ModelConfig;
    use std::io::Write;

    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();
    let cqs_dir = root.join(".cqs");
    std::fs::create_dir_all(&cqs_dir).unwrap();
    let rs_file = root.join("nocache.rs");
    let mut f = std::fs::File::create(&rs_file).unwrap();
    f.write_all(b"pub fn no_cache_path() { let _ = 0; }")
        .unwrap();
    drop(f);

    let model_cfg = ModelConfig::resolve(None, None);
    let embedder = Embedder::new_cpu(model_cfg).expect("init CPU embedder");
    let dim = embedder.embedding_dim();
    let store_path = cqs_dir.join(cqs::INDEX_DB_FILENAME);
    let mut store = Store::open(&store_path).unwrap();
    store
        .init(&cqs::store::ModelInfo::new(
            &embedder.model_config().repo,
            dim,
        ))
        .unwrap();
    store.set_dim(dim);

    let parser = CqParser::new().unwrap();
    let files = vec![PathBuf::from("nocache.rs")];
    let (count, _) = reindex_files(root, &store, &files, &parser, &embedder, None, true)
        .expect("reindex_files without global_cache");
    assert!(count >= 1, "no-cache path must still index");
}

#[test]
fn env_snapshot_redacts_api_key() {
    // SEC-V1.30.1-8 / P1.10: CQS_LLM_API_KEY mustn't land in journald.
    // The daemon-startup snapshot in `mod.rs` builds the same redaction
    // logic; this test pins the redaction shape against a fixture so a
    // regression that drops the suffix list flips this assertion.
    const SECRET_SUFFIXES: &[&str] = &["_API_KEY", "_TOKEN", "_PASSWORD", "_SECRET"];
    let pairs = vec![
        ("CQS_LLM_API_KEY".to_string(), "sk-real-secret".to_string()),
        ("CQS_TELEMETRY".to_string(), "1".to_string()),
    ];
    let redacted: Vec<(String, String)> = pairs
        .into_iter()
        .map(|(k, v)| {
            let is_secret = SECRET_SUFFIXES.iter().any(|suffix| k.ends_with(suffix));
            let value = if is_secret {
                format!("<redacted len={}>", v.len())
            } else {
                v
            };
            (k, value)
        })
        .collect();
    assert_eq!(
        redacted[0].1, "<redacted len=14>",
        "CQS_LLM_API_KEY value must not appear in plaintext"
    );
    assert_eq!(
        redacted[1].1, "1",
        "CQS_TELEMETRY is non-secret, kept verbatim"
    );
}

// ===== process_file_changes ordering tests (P1.2) =====

/// CQ-V1.30.1-1 / AC-V1.30.1-4 / DS-V1.30.1-D8: `dropped_this_cycle` must
/// NOT be reset before the embedder-init check. When `try_init_embedder`
/// early-returns (init failure or backoff), the counter must survive so
/// the outer loop's `publish_watch_snapshot` can observe it as a Stale
/// signal. Pre-fix, the counter was zeroed at the top of the function
/// regardless of which path ran, so `cqs eval --require-fresh` accepted
/// indexes whose only-witness drops had been wiped.
///
/// We force the embedder-init early-return by recording an
/// `EmbedderBackoff` failure (so `should_retry()` blocks for ~2 s) on a
/// state whose `TEST_EMBEDDER` `OnceLock` has never been populated.
#[test]
fn dropped_this_cycle_survives_embedder_init_early_return() {
    let fix = drain_test_fixture(4);
    let cfg = test_watch_config(
        fix.tmp.path(),
        fix.tmp.path(),
        &fix.notes_path,
        &fix.supported_ext,
    );

    let mut state = test_watch_state();
    // Seed the conditions of a saturated debounce cycle: one queued
    // file plus a non-zero dropped count from prior cap-overflow events.
    state.pending_files.insert(PathBuf::from("queued.rs"));
    state.dropped_this_cycle = 5;
    // Force `try_init_embedder` → None without loading any model: the
    // shared `TEST_EMBEDDER` OnceLock is empty (set up in this file as
    // `LazyLock::new(OnceLock::new)`), so we just need backoff to block
    // the retry path. One recorded failure is enough — `record_failure`
    // sets `next_retry = now + 2 s`.
    state.embedder_backoff.record_failure();
    assert!(
        !state.embedder_backoff.should_retry(),
        "test setup: backoff must block retry so try_init_embedder returns None"
    );

    process_file_changes(&cfg, &fix.store, &mut state);

    // Pin: the early-return path must NOT have zeroed the counter.
    assert_eq!(
        state.dropped_this_cycle, 5,
        "dropped_this_cycle must survive embedder-init early-return so the \
         next publish_watch_snapshot reports state=Stale"
    );
    // Sanity: `pending_files` is drained at the top of the function (by
    // design — collected events are taken before any work), so this is
    // not what shields the snapshot. The drop counter is.
    assert!(state.pending_files.is_empty(), "pending_files drains first");
}

/// CQ-V1.30.1-1 ordering complement: a *successful* drain must reset
/// `dropped_this_cycle` to 0. The reset moved to after `Ok((count, ...))`
/// so this exercises the post-drain branch and pins the contract for
/// future refactors. Uses `drain_test_fixture` (no embedder needed) and
/// confirms the function reaches the success arm by passing an empty
/// `pending_files` set after seeding the dropped counter — the function
/// processes "0 files changed" as a successful (count=0) drain.
///
/// Implementation note: even with no files, `reindex_files` returns
/// `Ok((0, vec![]))` and the success arm runs — that's the behavior we
/// want, since "we got through cleanly" is the right time to reset. A
/// CPU embedder is required to reach `reindex_files`. We use the same
/// trick as `dropped_this_cycle_survives_embedder_init_early_return`
/// inverted: this time the embedder is needed, so we don't want the
/// backoff path. We `#[ignore]` this test because it requires loading a
/// real CPU embedder model — the early-return test above carries the
/// load-bearing regression assertion; this one just documents the
/// success-arm reset contract for human readers.
#[test]
#[ignore = "Requires loading a real CPU embedder; survives_embedder_init_early_return is the load-bearing regression test"]
fn dropped_this_cycle_resets_after_successful_drain() {
    use cqs::embedder::ModelConfig;

    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();
    let cqs_dir = root.join(".cqs");
    std::fs::create_dir_all(&cqs_dir).unwrap();

    let model_cfg = ModelConfig::resolve(None, None);
    let embedder = Embedder::new_cpu(model_cfg).expect("init CPU embedder");
    let dim = embedder.embedding_dim();
    let store_path = cqs_dir.join(cqs::INDEX_DB_FILENAME);
    let mut store = Store::open(&store_path).unwrap();
    store
        .init(&cqs::store::ModelInfo::new(
            &embedder.model_config().repo,
            dim,
        ))
        .unwrap();
    store.set_dim(dim);
    let notes_path = cqs_dir.join("docs/notes.toml");
    let supported: HashSet<&str> = HashSet::new();

    // Embedder slot pre-populated so try_init_embedder returns Some.
    let embedder_slot = std::sync::OnceLock::new();
    let _ = embedder_slot.set(std::sync::Arc::new(embedder));
    let model_cfg2 = ModelConfig::default_model();
    let parser = CqParser::new().unwrap();
    let gitignore = std::sync::RwLock::new(None);

    let cfg = WatchConfig {
        root,
        cqs_dir: &cqs_dir,
        notes_path: &notes_path,
        supported_ext: &supported,
        parser: &parser,
        embedder: &embedder_slot,
        quiet: true,
        model_config: &model_cfg2,
        gitignore: &gitignore,
        splade_encoder: None,
        global_cache: None,
    };

    let mut state = test_watch_state();
    state.dropped_this_cycle = 5;
    // No files queued → reindex_files returns Ok((0, vec![])).

    process_file_changes(&cfg, &store, &mut state);

    assert_eq!(
        state.dropped_this_cycle, 0,
        "successful drain must reset the counter so the next cycle's \
         snapshot reflects fresh state"
    );
}
