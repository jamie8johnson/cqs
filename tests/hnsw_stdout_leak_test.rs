//! Regression guard: building an HNSW must NOT leak anything to the process
//! stdout.
//!
//! ## The bug
//!
//! hnsw_rs 0.3.4's `modify_level_scale` unconditionally `println!`s a
//! `"Current scale value : ..."` diagnostic line to the process stdout. cqs
//! applies the reduced level scale via that method at HNSW *construction*, on
//! both metric arms, before any point is inserted — so every index build emitted
//! that line. For commands that write machine-readable JSON to stdout (notably
//! `cqs gc --json`, which rebuilds the HNSW and then writes its JSON summary)
//! the stray line corrupted the JSON contract. The fix wraps the
//! `modify_level_scale` calls in a cross-platform stdout suppressor
//! (`src/hnsw/stdout_gag.rs`).
//!
//! ## Why a subprocess instead of a plain in-process capture
//!
//! The leak is at the OS `fd 1` level. The obvious in-process test — install a
//! fd-1 capture (a `dup`/`dup2` redirect to a pipe) around an
//! `HnswIndex::build_*` call and assert the capture is empty — is a FALSE guard
//! under normal
//! `cargo test`: libtest installs a thread-local Rust-stdout capture
//! (`std::io::set_output_capture`) that intercepts `println!` BEFORE it reaches
//! `fd 1`, so the fd-1 capture sees nothing and the assertion passes even with
//! the gag removed (it only bites under `--nocapture`). Spawning a child thread
//! does not reliably escape that interception either.
//!
//! A child PROCESS has no libtest stdout capture in effect for its workload, so
//! its `println!` flows to the real `fd 1` exactly as the production `cqs`
//! binary's does. We therefore re-exec THIS test binary, ask it to run only the
//! `child_build_hnsw_writes_nothing_to_stdout` workload under `--nocapture`, and
//! assert the child's piped stdout is byte-for-byte empty. This bites in PR CI:
//! removing the gag makes the child leak "Current scale value", failing the
//! parent's emptiness assertion. It is fast — no embedder, no `cqs index`.

use std::process::Command;

/// Env flag the parent sets so the re-exec'd child knows to actually run the
/// workload (rather than re-spawn another child, which would recurse).
/// `CQS_TEST_` prefix per the test-only env-var convention enforced by
/// `tests/env_var_docs.rs` (such vars are exempt from README documentation).
const CHILD_FLAG: &str = "CQS_TEST_HNSW_STDOUT_LEAK_CHILD";

/// CHILD WORKLOAD (only meaningful when re-exec'd by the parent below).
///
/// Builds an HNSW via the public `HnswIndex::build_with_dim` API, which routes
/// through `HnswGraph::new` and thus calls `modify_level_scale` on the active
/// metric arm — the exact production construction path. When run as the
/// re-exec'd child under `--nocapture`, any leaked diagnostic line lands on the
/// child's real stdout, where the parent captures it.
///
/// When run directly (not via the parent, i.e. the env flag is unset) this is a
/// no-op so it never disturbs a normal `cargo test` run on its own.
#[test]
fn child_build_hnsw_writes_nothing_to_stdout() {
    if std::env::var_os(CHILD_FLAG).is_none() {
        // Direct run (not the re-exec'd child): do nothing. The parent test
        // below drives the real assertion.
        return;
    }

    // Build a small index. Distinct unit-ish vectors so the build is valid;
    // exact contents are irrelevant — we only care that construction stays
    // silent on stdout.
    let dim = cqs::EMBEDDING_DIM;
    let embeddings: Vec<(String, cqs::Embedding)> = (0..4)
        .map(|i| {
            let mut v = vec![0.0f32; dim];
            v[i % dim] = 1.0;
            (format!("chunk_{i}"), cqs::Embedding::new(v))
        })
        .collect();

    let _index = cqs::hnsw::HnswIndex::build_with_dim(embeddings, dim)
        .expect("HNSW build failed in child workload");

    // Deliberately write NOTHING to stdout here. If construction is silent (gag
    // working) the child's stdout is empty; if it leaked, the diagnostic line is
    // already on stdout. Either way we exit cleanly so the parent inspects the
    // captured stdout, not our exit status.
}

/// PARENT: re-exec this test binary to run only the child workload under
/// `--nocapture`, then assert the child produced NO stdout. Empty stdout ⇒ the
/// stdout gag suppressed hnsw_rs's `modify_level_scale` `println!`.
#[test]
fn hnsw_construction_does_not_leak_to_stdout() {
    let exe = std::env::current_exe().expect("failed to resolve test binary path");

    let output = Command::new(&exe)
        // Run ONLY the child workload, capturing its real stdout. `--nocapture`
        // ensures libtest does not intercept the child's println! before it
        // reaches fd 1 (the layer the production leak occurs at and the gag
        // suppresses).
        .args([
            "--exact",
            "child_build_hnsw_writes_nothing_to_stdout",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_FLAG, "1")
        // Keep the child's own diagnostics off our captured stdout if anything
        // changes upstream: route any cqs tracing/logging (already stderr) and
        // ensure no env-driven verbosity sneaks onto stdout.
        .env_remove("RUST_LOG")
        .output()
        .expect("failed to re-exec test binary for child workload");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // libtest itself prints its run summary ("running 1 test", "test result:
    // ok.", etc.) to the child's STDOUT. Those framing lines are libtest's, not
    // our workload's leak, so we filter them out and assert nothing ELSE
    // remains. The leak we guard against is hnsw_rs's "Current scale value"
    // line; assert it specifically is absent, and that no non-libtest content
    // slips through.
    assert!(
        !stdout.contains("Current scale value"),
        "HNSW construction leaked hnsw_rs's modify_level_scale diagnostic to \
         stdout (the StdoutGag is not suppressing it).\n--- child stdout ---\n{stdout}\n\
         --- child stderr ---\n{stderr}"
    );

    let leaked: Vec<&str> = stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| !is_libtest_framing(line))
        .collect();
    assert!(
        leaked.is_empty(),
        "HNSW construction leaked unexpected content to stdout: {leaked:?}\n\
         --- full child stdout ---\n{stdout}\n--- child stderr ---\n{stderr}"
    );

    // Sanity: the child must have actually run the workload (exit success),
    // otherwise an empty stdout would be a vacuous pass.
    assert!(
        output.status.success(),
        "child workload did not succeed (status {:?}); stderr:\n{stderr}",
        output.status.code()
    );
}

/// True for the framing lines libtest writes to stdout around a test run, which
/// are not our workload's output. Anything that is NOT framing and NOT empty is
/// treated as a leak.
fn is_libtest_framing(line: &str) -> bool {
    line.starts_with("running ")
        || line.starts_with("test result:")
        || line.starts_with("test child_build_hnsw_writes_nothing_to_stdout")
        // The bare "test <name> ... ok" / "... ignored" status line.
        || (line.starts_with("test ") && (line.ends_with("ok") || line.ends_with("ignored")))
}
