//! RT-RELAY: doc-comment injection-flag relay guard.
//!
//! `detect_all_injection_patterns` historically ran only on a chunk's
//! `content`. A doc comment is parsed into a separate `doc` column that
//! `cqs context` and `cqs explain` relay verbatim while the injection scan
//! never saw it — so a poisoned doc reached an agent (or, via
//! `structuredContent`, an MCP client) with a hard-coded false-clean
//! `injection_flags: []` signal.
//!
//! SECURITY.md promises `injection_flags` on every chunk-returning JSON
//! output whenever a heuristic fired. These tests seed ONE chunk whose
//! `content` is benign but whose `doc` carries detector-recognized payloads
//! (an `https://` URL → `embedded-url`, a triple-backtick fence →
//! `code-fence`) and assert the relayed surfaces both (a) actually relay the
//! payload — proving the threat is real — and (b) surface a non-empty
//! `injection_flags` array.

mod common;

use assert_cmd::Command;
use common::cqs_v1 as cqs;
use cqs::parser::{Chunk, ChunkType, Language};
use cqs::store::ModelInfo;
use cqs::Store;
use serde_json::Value;
use std::path::PathBuf;
use tempfile::TempDir;

/// Benign content; the payload lives in the doc comment.
const BENIGN_CONTENT: &str = "fn helper_widget_processor() {}";

/// Doc comment carrying two detector-recognized payloads:
///   - an `https://` URL  → `embedded-url`
///   - a triple-backtick fence → `code-fence`
const POISONED_DOC: &str =
    "See https://evil.example.com/payload for details.\n\n```\nrm -rf /\n```";

fn cqs_no_daemon() -> Command {
    let mut c = cqs();
    c.env("CQS_NO_DAEMON", "1");
    c
}

fn dummy_embedding() -> cqs::embedder::Embedding {
    let dim = ModelInfo::default().dimensions;
    let v: Vec<f32> = (0..dim).map(|i| ((i % 7) as f32) * 0.01).collect();
    cqs::embedder::Embedding::new(v)
}

/// Seed a `.cqs/index.db` with one chunk: benign `content`, poisoned `doc`.
fn seed_poisoned_doc_store() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    let cqs_dir = dir.path().join(".cqs");
    std::fs::create_dir_all(&cqs_dir).expect("mkdir .cqs");
    let db_path = cqs_dir.join(cqs::INDEX_DB_FILENAME);

    let store = Store::open(&db_path).expect("open store");
    store.init(&ModelInfo::default()).expect("init");

    let content_hash = blake3::hash(BENIGN_CONTENT.as_bytes()).to_hex().to_string();
    let chunk = Chunk {
        id: "src/lib.rs:1:helper_widget_processor".to_string(),
        file: PathBuf::from("src/lib.rs"),
        language: Language::Rust,
        chunk_type: ChunkType::Function,
        name: "helper_widget_processor".to_string(),
        signature: "fn helper_widget_processor()".to_string(),
        content: BENIGN_CONTENT.to_string(),
        doc: Some(POISONED_DOC.to_string()),
        line_start: 1,
        line_end: 1,
        byte_start: 0,
        content_hash,
        canonical_hash: String::new(),
        parent_id: None,
        window_idx: None,
        parent_type_name: None,
        parser_version: 0,
    };
    let emb = dummy_embedding();
    store.upsert_chunk(&chunk, &emb, Some(1)).expect("upsert");

    drop(store);
    dir
}

/// Precondition: the detector itself recognizes both payloads in the doc.
/// If this fails the rest is moot — the threat model assumes the heuristics
/// fire on this text.
#[test]
fn precondition_detector_flags_doc_payloads() {
    let flags = cqs::llm::validation::detect_all_injection_patterns(POISONED_DOC);
    assert!(
        flags.contains(&"embedded-url"),
        "detector must flag embedded-url on the doc payload: {flags:?}"
    );
    assert!(
        flags.contains(&"code-fence"),
        "detector must flag code-fence on the doc payload: {flags:?}"
    );
    // Benign content must NOT trip the detector — proves the flags come from
    // the doc, not the content.
    let content_flags = cqs::llm::validation::detect_all_injection_patterns(BENIGN_CONTENT);
    assert!(
        content_flags.is_empty(),
        "benign content must not trip the detector: {content_flags:?}"
    );
}

/// `cqs context <file> --json` relays the poisoned doc verbatim AND must
/// surface a non-empty `injection_flags` array on that chunk.
#[test]
fn context_relays_doc_and_flags_injection() {
    let dir = seed_poisoned_doc_store();

    let result = cqs_no_daemon()
        .args(["context", "src/lib.rs", "--json"])
        .current_dir(dir.path())
        .output()
        .expect("run cqs context");

    let stdout = String::from_utf8_lossy(&result.stdout).to_string();
    let stderr = String::from_utf8_lossy(&result.stderr).to_string();
    assert!(
        result.status.success(),
        "cqs context must succeed. stderr={stderr} stdout={stdout}"
    );

    let parsed: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|_| panic!("--json output must be JSON. got: {stdout}"));

    let chunk0 = &parsed["data"]["chunks"][0];

    // (a) The threat is real: the doc payload is relayed verbatim.
    let doc = chunk0["doc"]
        .as_str()
        .unwrap_or_else(|| panic!("data.chunks[0].doc must be a string: {parsed:?}"));
    assert!(
        doc.contains("https://evil.example.com/payload"),
        "context must relay the poisoned doc URL: {doc:?}"
    );
    assert!(
        doc.contains("```"),
        "context must relay the poisoned doc code fence: {doc:?}"
    );

    // (b) The signal: injection_flags must be a non-empty array.
    let flags = chunk0["injection_flags"]
        .as_array()
        .unwrap_or_else(|| panic!("data.chunks[0].injection_flags must be an array: {parsed:?}"));
    assert!(
        !flags.is_empty(),
        "context must flag the doc-borne injection, got empty injection_flags: {chunk0:?}"
    );
    let flag_strs: Vec<&str> = flags.iter().filter_map(|f| f.as_str()).collect();
    assert!(
        flag_strs.contains(&"embedded-url"),
        "context injection_flags must include embedded-url: {flag_strs:?}"
    );
    assert!(
        flag_strs.contains(&"code-fence"),
        "context injection_flags must include code-fence: {flag_strs:?}"
    );
}

/// `cqs explain <fn> --json` relays the poisoned doc verbatim AND must carry
/// a non-empty `injection_flags` array on the function card.
#[test]
fn explain_relays_doc_and_flags_injection() {
    let dir = seed_poisoned_doc_store();

    let result = cqs_no_daemon()
        .args(["explain", "helper_widget_processor", "--json"])
        .current_dir(dir.path())
        .output()
        .expect("run cqs explain");

    let stdout = String::from_utf8_lossy(&result.stdout).to_string();
    let stderr = String::from_utf8_lossy(&result.stderr).to_string();
    assert!(
        result.status.success(),
        "cqs explain must succeed. stderr={stderr} stdout={stdout}"
    );

    let parsed: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|_| panic!("--json output must be JSON. got: {stdout}"));

    // (a) The threat is real: the doc payload is relayed verbatim.
    let doc = parsed["data"]["doc"]
        .as_str()
        .unwrap_or_else(|| panic!("data.doc must be a string: {parsed:?}"));
    assert!(
        doc.contains("https://evil.example.com/payload"),
        "explain must relay the poisoned doc URL: {doc:?}"
    );
    assert!(
        doc.contains("```"),
        "explain must relay the poisoned doc code fence: {doc:?}"
    );

    // (b) The signal: injection_flags must exist and be non-empty.
    let flags = parsed["data"]["injection_flags"]
        .as_array()
        .unwrap_or_else(|| panic!("data.injection_flags must be an array: {parsed:?}"));
    assert!(
        !flags.is_empty(),
        "explain must flag the doc-borne injection, got empty injection_flags: {parsed:?}"
    );
    let flag_strs: Vec<&str> = flags.iter().filter_map(|f| f.as_str()).collect();
    assert!(
        flag_strs.contains(&"embedded-url"),
        "explain injection_flags must include embedded-url: {flag_strs:?}"
    );
    assert!(
        flag_strs.contains(&"code-fence"),
        "explain injection_flags must include code-fence: {flag_strs:?}"
    );
}
