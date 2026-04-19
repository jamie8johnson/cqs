//! Integration tests for cross-project call graph queries.

use std::path::PathBuf;

use cqs::cross_project::{CrossProjectContext, NamedStore};
use cqs::parser::{ChunkType, Language};
use cqs::store::{ModelInfo, ReadOnly, ReadWrite, StoreError};
use cqs::Store;
use tempfile::TempDir;

/// Build a read-only `Store` at `dir/index.db`, initializing it and running
/// `populate` for fixture setup. Wraps [`Store::open_readonly_after_init`]
/// so tests don't have to repeat the init boilerplate.
fn build_readonly_store<F>(dir: &TempDir, populate: F) -> Store<ReadOnly>
where
    F: FnOnce(&Store<ReadWrite>) -> Result<(), StoreError>,
{
    let db_path = dir.path().join(cqs::INDEX_DB_FILENAME);
    Store::<ReadOnly>::open_readonly_after_init(&db_path, |store| {
        store.init(&ModelInfo::default())?;
        populate(store)
    })
    .expect("open_readonly_after_init")
}

fn insert_chunk_and_call(
    store: &Store<ReadWrite>,
    caller: &str,
    callee: &str,
    file: &str,
) -> Result<(), StoreError> {
    // Use the public API: upsert_chunks + upsert_function_calls
    let chunk = cqs::Chunk {
        id: format!("{}:1:hash_{}", file, caller),
        file: PathBuf::from(file),
        language: Language::Rust,
        chunk_type: ChunkType::Function,
        name: caller.to_string(),
        signature: format!("fn {}()", caller),
        content: format!("fn {}() {{ {}(); }}", caller, callee),
        doc: None,
        line_start: 1,
        line_end: 5,
        content_hash: format!("hash_{}", caller),
        window_idx: None,
        parent_id: None,
        parent_type_name: None,
        parser_version: 0,
    };
    let embedding = cqs::Embedding::new(vec![0.0f32; store.dim()]);
    store.upsert_chunks_batch(&[(chunk, embedding)], None)?;

    let calls = vec![cqs::parser::FunctionCalls {
        name: caller.to_string(),
        line_start: 1,
        calls: vec![cqs::parser::CallSite {
            callee_name: callee.to_string(),
            line_number: 3,
        }],
    }];
    store.upsert_function_calls(&PathBuf::from(file), &calls)?;
    Ok(())
}

#[test]
fn test_cross_project_callers_finds_both() {
    let dir_a = TempDir::new().unwrap();
    let dir_b = TempDir::new().unwrap();
    let store_a = build_readonly_store(&dir_a, |s| {
        insert_chunk_and_call(s, "foo", "target", "a.rs")
    });
    let store_b = build_readonly_store(&dir_b, |s| {
        insert_chunk_and_call(s, "bar", "target", "b.rs")
    });

    let mut ctx = CrossProjectContext::new(vec![
        NamedStore {
            name: "local".into(),
            store: store_a,
        },
        NamedStore {
            name: "project_b".into(),
            store: store_b,
        },
    ]);

    let callers = ctx.get_callers_cross("target").unwrap();
    assert!(
        callers.len() >= 2,
        "Expected callers from both projects, got {}",
        callers.len()
    );
    let projects: Vec<&str> = callers.iter().map(|c| c.project.as_str()).collect();
    assert!(projects.contains(&"local"));
    assert!(projects.contains(&"project_b"));
}

#[test]
fn test_cross_project_callees_finds_both() {
    let dir_a = TempDir::new().unwrap();
    let dir_b = TempDir::new().unwrap();
    let store_a = build_readonly_store(&dir_a, |s| {
        insert_chunk_and_call(s, "source", "foo", "a.rs")
    });
    let store_b = build_readonly_store(&dir_b, |s| {
        insert_chunk_and_call(s, "source", "bar", "b.rs")
    });

    let mut ctx = CrossProjectContext::new(vec![
        NamedStore {
            name: "local".into(),
            store: store_a,
        },
        NamedStore {
            name: "project_b".into(),
            store: store_b,
        },
    ]);

    let callees = ctx.get_callees_cross("source").unwrap();
    assert!(
        callees.len() >= 2,
        "Expected callees from both projects, got {}",
        callees.len()
    );
}

#[test]
fn test_cross_project_no_references_local_only() {
    let dir = TempDir::new().unwrap();
    let store = build_readonly_store(&dir, |s| insert_chunk_and_call(s, "foo", "target", "a.rs"));

    let mut ctx = CrossProjectContext::new(vec![NamedStore {
        name: "local".into(),
        store,
    }]);

    let callers = ctx.get_callers_cross("target").unwrap();
    assert_eq!(callers.len(), 1);
    assert_eq!(callers[0].project, "local");
}

#[test]
fn test_cross_project_function_not_found() {
    let dir = TempDir::new().unwrap();
    let store = build_readonly_store(&dir, |_s| Ok(()));

    let mut ctx = CrossProjectContext::new(vec![NamedStore {
        name: "local".into(),
        store,
    }]);

    let callers = ctx.get_callers_cross("nonexistent").unwrap();
    assert!(callers.is_empty());
}

#[test]
fn test_cross_project_same_name_different_sources() {
    let dir_a = TempDir::new().unwrap();
    let dir_b = TempDir::new().unwrap();
    let store_a = build_readonly_store(&dir_a, |s| {
        insert_chunk_and_call(s, "init", "target", "a.rs")
    });
    let store_b = build_readonly_store(&dir_b, |s| {
        insert_chunk_and_call(s, "init", "target", "b.rs")
    });

    let mut ctx = CrossProjectContext::new(vec![
        NamedStore {
            name: "local".into(),
            store: store_a,
        },
        NamedStore {
            name: "project_b".into(),
            store: store_b,
        },
    ]);

    let callers = ctx.get_callers_cross("target").unwrap();
    assert_eq!(callers.len(), 2);
    let projects: Vec<&str> = callers.iter().map(|c| c.project.as_str()).collect();
    assert!(projects.contains(&"local"));
    assert!(projects.contains(&"project_b"));
}
