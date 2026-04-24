//! TC-HAP-1.29-2 placeholder — the actual batch-handler smoke tests live in
//! `src/cli/batch/handlers/dispatch_tests.rs`, not here.
//!
//! Why: `BatchContext::dispatch_line` and `create_test_context` are
//! `pub(crate)` / `pub(in crate::cli)` inside the binary's `cli` module.
//! Integration tests in `tests/` link against the library crate only —
//! `src/main.rs` and everything under `src/cli/` are unreachable from here.
//!
//! Running the tests: `cargo test --features gpu-index --bin cqs
//! cli::batch::handlers::dispatch_tests`.
//!
//! The 11 tests cover: callers, callees, impact, test-map, trace, similar,
//! explain (no-tokens path), context (no-tokens path), deps, related,
//! impact-diff. The remaining 5 handlers (gather / scout / task / where /
//! onboard) require an ONNX cold-load and are deliberately skipped per
//! the audit contract that tests must not require model load.
