//! TC-ADV-1.29-3 placeholder — the actual daemon-socket adversarial tests
//! live in `src/cli/watch.rs` under `mod adversarial_socket_tests`.
//!
//! Why: `handle_socket_client` is a private `fn` inside the binary's
//! `cli::watch` module. Integration tests in `tests/` link against the
//! library crate only — they can't reach it. Co-locating with the
//! production code matches the precedent set by the existing `mod tests`
//! in `watch.rs` and keeps the pair-harness (UnixStream::pair + spawn
//! thread + drive client end) close to the thing it tests.
//!
//! Running the tests: `cargo test --features gpu-index --bin cqs
//! cli::watch::adversarial_socket_tests`.
//!
//! Coverage (8 tests):
//!   1. exactly 1 MiB + 1 byte → "request too large"
//!   2. malformed JSON with trailing garbage → "invalid JSON: ..."
//!   3. UTF-16 BOM prefix → current drop-without-response behaviour
//!   4. bare newline → "invalid JSON: EOF..."
//!   5. missing `command` field → "missing 'command' field"
//!   6. non-string args (objects, nulls, numbers) → rejected with the
//!      exact "args contains non-string elements" message (P3 #86)
//!   7. 500 KB single arg within the 1 MiB line cap → currently accepted,
//!      pins behaviour for a future per-arg-cap decision
//!   8. NUL byte in args → inner envelope surfaces `invalid_input`
//!      (reject_null_tokens in cli::batch::mod.rs)
