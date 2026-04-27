//! Integration tests for slot + embeddings cache (spec
//! `docs/plans/2026-04-24-embeddings-cache-and-slots.md`).
//!
//! These tests exercise the FS layout and orchestration logic without
//! invoking the binary CLI — the slot helpers in `cqs::slot` and the
//! cache helpers in `cqs::cache` are callable from test code directly,
//! and that's enough to cover the integration concerns spec'd in
//! §Testing (items 13–28).

use std::fs;
use std::path::Path;

use cqs::cache::{CachePurpose, EmbeddingCache};
use cqs::slot::{
    active_slot_path, list_slots, migrate_legacy_index_to_default_slot, read_active_slot,
    resolve_slot_name, slot_dir, write_active_slot, DEFAULT_SLOT,
};

/// §Testing #21 — `active_slot` references a missing slot dir → resolution
/// should still produce SOMETHING usable. Today: caller resolves to whatever
/// the file says; concrete error happens when `Store::open` is attempted.
/// This exercises the resolve_slot_name path.
#[test]
fn active_slot_references_missing_dir_resolution_succeeds_but_dir_absent() {
    let dir = tempfile::tempdir().unwrap();
    let cqs = dir.path().join(".cqs");
    fs::create_dir_all(&cqs).unwrap();
    write_active_slot(&cqs, "missing").unwrap();
    let r = resolve_slot_name(None, &cqs).unwrap();
    assert_eq!(r.name, "missing");
    // Validate the slot dir does not exist — caller's responsibility to
    // produce an actionable error when opening `Store` against this path.
    assert!(!slot_dir(&cqs, &r.name).exists());
}

/// §Testing #25 — surface area smoke for the rollback path: when migration
/// itself succeeds, all source files are moved AND the active_slot pointer
/// is written. Disk-full mid-migration is harder to fault-inject portably
/// (the EXDEV branch is exercised by [`std::fs::rename`] cross-FS calls,
/// not reproducible from a tempfile), so this test instead verifies the
/// happy path is fully consistent.
#[test]
fn migration_succeeds_then_all_files_moved_and_pointer_written() {
    let dir = tempfile::tempdir().unwrap();
    let cqs = dir.path().join(".cqs");
    fs::create_dir_all(&cqs).unwrap();
    fs::write(cqs.join("index.db"), b"db-data").unwrap();
    fs::write(cqs.join("index.hnsw.data"), b"hnsw-data").unwrap();
    fs::write(cqs.join("index.hnsw.graph"), b"hnsw-graph").unwrap();
    fs::write(cqs.join("splade.index.bin"), b"splade").unwrap();

    let did = migrate_legacy_index_to_default_slot(&cqs).unwrap();
    assert!(did);

    let dest = slot_dir(&cqs, DEFAULT_SLOT);
    for n in [
        "index.db",
        "index.hnsw.data",
        "index.hnsw.graph",
        "splade.index.bin",
    ] {
        assert!(dest.join(n).exists(), "{n} should be in slots/default/");
        assert!(!cqs.join(n).exists(), "{n} should NOT remain in .cqs/ root");
    }
    assert_eq!(read_active_slot(&cqs).as_deref(), Some(DEFAULT_SLOT));
}

/// §Testing #18 — cache holds (chunk_hash, model_id) pairs across slots.
/// Two slots with the same model_id and overlapping chunks share entries.
#[test]
fn cache_shared_across_slots_for_same_model_id() {
    let dir = tempfile::tempdir().unwrap();
    let cache_path = EmbeddingCache::project_default_path(dir.path());
    let cache = EmbeddingCache::open(&cache_path).unwrap();

    // Pretend slot "a" embeds 5 chunks with model "bge".
    let entries: Vec<(String, Vec<f32>)> = (0..5)
        .map(|i| (format!("h{i}"), vec![i as f32, 0.0, 0.0]))
        .collect();
    cache
        .write_batch_owned(&entries, "bge", CachePurpose::Embedding, 3)
        .unwrap();

    // Slot "b" with the same model_id queries the same hashes; partition
    // returns hits for ALL of them — no re-embed needed.
    let items: Vec<&str> = (0..5)
        .map(|i| Box::leak(format!("h{i}").into_boxed_str()) as &str)
        .collect();
    let (cached, missed) = cache
        .partition(&items, "bge", CachePurpose::Embedding, 3, |s: &&str| *s)
        .unwrap();
    assert_eq!(cached.len(), 5);
    assert!(missed.is_empty());
}

/// §Testing #20 — `prune_by_model` only removes entries for the named model.
#[test]
fn cache_prune_by_model_isolated() {
    let dir = tempfile::tempdir().unwrap();
    let cache_path = EmbeddingCache::project_default_path(dir.path());
    let cache = EmbeddingCache::open(&cache_path).unwrap();
    let entries: Vec<(String, Vec<f32>)> = (0..3)
        .map(|i| (format!("hh{i}"), vec![i as f32; 4]))
        .collect();
    cache
        .write_batch_owned(&entries, "alpha", CachePurpose::Embedding, 4)
        .unwrap();
    cache
        .write_batch_owned(&entries, "beta", CachePurpose::Embedding, 4)
        .unwrap();

    let removed = cache.prune_by_model("alpha").unwrap();
    assert_eq!(removed, 3);

    // beta survives
    let stats = cache.stats_per_model().unwrap();
    assert_eq!(stats.len(), 1);
    assert_eq!(stats[0].model_id, "beta");
}

/// §Testing #19 — `cqs cache stats` reflects entries after writing.
#[test]
fn cache_stats_reflect_inserted_entries() {
    let dir = tempfile::tempdir().unwrap();
    let cache_path = EmbeddingCache::project_default_path(dir.path());
    let cache = EmbeddingCache::open(&cache_path).unwrap();
    let entries: Vec<(String, Vec<f32>)> = (0..7)
        .map(|i| (format!("z{i}"), vec![i as f32; 8]))
        .collect();
    cache
        .write_batch_owned(&entries, "model_q", CachePurpose::Embedding, 8)
        .unwrap();
    let s = cache.stats().unwrap();
    assert_eq!(s.total_entries, 7);
    assert_eq!(s.unique_models, 1);
}

/// §Testing #15 — incremental reindex flow: cache hits on a re-run with
/// identical content produce 100% hit rate.
#[test]
fn cache_partition_full_hit_on_identical_rerun() {
    let dir = tempfile::tempdir().unwrap();
    let cache_path = EmbeddingCache::project_default_path(dir.path());
    let cache = EmbeddingCache::open(&cache_path).unwrap();

    // First run: embed.
    let entries: Vec<(String, Vec<f32>)> = (0..50)
        .map(|i| (format!("c{i}"), vec![i as f32; 16]))
        .collect();
    cache
        .write_batch_owned(&entries, "m1", CachePurpose::Embedding, 16)
        .unwrap();

    // Second run: same hashes, same model — partition reports 100% hits.
    let items: Vec<String> = (0..50).map(|i| format!("c{i}")).collect();
    let item_refs: Vec<&str> = items.iter().map(|s| s.as_str()).collect();
    let (cached, missed) = cache
        .partition(&item_refs, "m1", CachePurpose::Embedding, 16, |s: &&str| *s)
        .unwrap();
    assert_eq!(cached.len(), 50);
    assert!(missed.is_empty());
}

/// Slot listing produces sorted, validated names — even if a junk dir is
/// dropped under `.cqs/slots/`.
#[test]
fn slot_list_filters_invalid_dir_names() {
    let dir = tempfile::tempdir().unwrap();
    let cqs = dir.path().join(".cqs");
    for n in ["good_one", "good-two"] {
        fs::create_dir_all(slot_dir(&cqs, n)).unwrap();
    }
    fs::create_dir_all(cqs.join("slots").join("BadName")).unwrap();
    fs::create_dir_all(cqs.join("slots").join("with space")).unwrap();
    let names = list_slots(&cqs).unwrap();
    assert_eq!(names, vec!["good-two".to_string(), "good_one".to_string()]);
}

/// §Testing #16 — two slots with different dims peacefully coexist on disk.
/// (Can't easily verify HNSW size differential without running the indexer,
/// but the FS structure must support it.)
#[test]
fn slot_dirs_can_hold_independent_index_dbs() {
    let dir = tempfile::tempdir().unwrap();
    let cqs = dir.path().join(".cqs");
    let bge = slot_dir(&cqs, "bge");
    let e5 = slot_dir(&cqs, "e5");
    fs::create_dir_all(&bge).unwrap();
    fs::create_dir_all(&e5).unwrap();
    fs::write(bge.join(cqs::INDEX_DB_FILENAME), b"fake-bge-data").unwrap();
    fs::write(e5.join(cqs::INDEX_DB_FILENAME), b"fake-e5-data").unwrap();

    write_active_slot(&cqs, "bge").unwrap();
    let r = resolve_slot_name(None, &cqs).unwrap();
    assert_eq!(r.name, "bge");

    write_active_slot(&cqs, "e5").unwrap();
    let r = resolve_slot_name(None, &cqs).unwrap();
    assert_eq!(r.name, "e5");
}

/// §Testing #27 — concurrent promote: two writes serialize via atomic
/// rename. After both calls return, the file content is from the last
/// call. This test simulates back-to-back promotes.
#[test]
fn concurrent_promote_last_writer_wins() {
    let dir = tempfile::tempdir().unwrap();
    let cqs = dir.path().join(".cqs");
    fs::create_dir_all(slot_dir(&cqs, "x")).unwrap();
    fs::create_dir_all(slot_dir(&cqs, "y")).unwrap();
    write_active_slot(&cqs, "x").unwrap();
    write_active_slot(&cqs, "y").unwrap();
    write_active_slot(&cqs, "x").unwrap();
    write_active_slot(&cqs, "y").unwrap();
    assert_eq!(read_active_slot(&cqs).as_deref(), Some("y"));
}

/// active_slot pointer atomic write: writing should not leave a partial
/// `.tmp` file in place when the write succeeds.
#[test]
fn active_slot_write_cleans_up_tmp() {
    let dir = tempfile::tempdir().unwrap();
    let cqs = dir.path().join(".cqs");
    write_active_slot(&cqs, "abc").unwrap();
    assert_eq!(read_active_slot(&cqs).as_deref(), Some("abc"));
    let tmp = cqs.join("active_slot.tmp");
    assert!(!tmp.exists(), "tmp file leaked: {}", tmp.display());
}

/// `active_slot_path` matches what `read_active_slot` reads from.
#[test]
fn active_slot_path_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let cqs = dir.path().join(".cqs");
    fs::create_dir_all(&cqs).unwrap();
    let p = active_slot_path(&cqs);
    assert_eq!(p, cqs.join("active_slot"));
    fs::write(&p, b"manual_write").unwrap();
    assert_eq!(read_active_slot(&cqs).as_deref(), Some("manual_write"));
}

/// Migration finalizes by writing `active_slot = "default"`.
#[test]
fn migration_writes_active_slot_pointer() {
    let dir = tempfile::tempdir().unwrap();
    let cqs = dir.path().join(".cqs");
    fs::create_dir_all(&cqs).unwrap();
    fs::write(cqs.join(cqs::INDEX_DB_FILENAME), b"data").unwrap();
    let did = migrate_legacy_index_to_default_slot(&cqs).unwrap();
    assert!(did);
    assert_eq!(read_active_slot(&cqs).as_deref(), Some(DEFAULT_SLOT));
}

/// `cqs::resolve_slot_dir` is a public, idempotent path computation.
#[test]
fn resolve_slot_dir_matches_internal_helper() {
    let p = cqs::resolve_slot_dir(Path::new("/proj/.cqs"), "alpha");
    assert_eq!(p, Path::new("/proj/.cqs/slots/alpha"));
}

/// `EmbeddingCache::project_default_path` returns the spec's sibling layout.
#[test]
fn embeddings_cache_project_default_path_in_cqs_dir() {
    let p = EmbeddingCache::project_default_path(Path::new("/proj/.cqs"));
    assert_eq!(p, Path::new("/proj/.cqs/embeddings_cache.db"));
}

/// `partition` against an empty slice should not fault even when the cache
/// has unrelated entries.
#[test]
fn cache_partition_empty_input_does_not_touch_db() {
    let dir = tempfile::tempdir().unwrap();
    let cache_path = EmbeddingCache::project_default_path(dir.path());
    let cache = EmbeddingCache::open(&cache_path).unwrap();
    cache
        .write_batch_owned(
            &[("h".to_string(), vec![1.0; 4])],
            "anything",
            CachePurpose::Embedding,
            4,
        )
        .unwrap();
    let items: Vec<&str> = Vec::new();
    let (c, m) = cache
        .partition(
            &items,
            "anything",
            CachePurpose::Embedding,
            4,
            |s: &&str| *s,
        )
        .unwrap();
    assert!(c.is_empty());
    assert!(m.is_empty());
}

/// `cache.compact()` is idempotent and survives re-runs on an empty cache.
#[test]
fn cache_compact_idempotent_on_empty() {
    let dir = tempfile::tempdir().unwrap();
    let cache_path = EmbeddingCache::project_default_path(dir.path());
    let cache = EmbeddingCache::open(&cache_path).unwrap();
    cache.compact().unwrap();
    cache.compact().unwrap();
}
