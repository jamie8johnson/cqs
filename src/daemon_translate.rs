//! CLI-argv to batch-request translation and daemon-socket path helpers.
//!
//! Pure arg-shaping logic extracted from `cli::dispatch::try_daemon_query` so
//! integration tests (`tests/daemon_forward_test.rs`) can exercise it without
//! reaching into the binary-only `cli` module tree. See issue #972.
//!
//! Also exposes `daemon_socket_path` (the real function used by the CLI) so
//! tests can compute the exact socket path for a given `cqs_dir` and bind a
//! mock `UnixListener` there.
//!
//! The daemon speaks the same syntax as `cqs batch`: one JSON object per line
//! with `{"command": "<sub>", "args": [...]}`. The CLI args that reach
//! `try_daemon_query` are the raw argv post-`cqs` (i.e. `std::env::args()
//! .skip(1)`) — still mixed with global flags the top-level `Cli` struct
//! consumes (`--json`, `-q`, `--model VAL`) and sub-command flags that the
//! batch parser consumes on the subcommand side.
//!
//! Responsibilities:
//!
//! - Strip global boolean flags (`--json`, `-q`, `--quiet`) — they live on
//!   `Cli` not on the subcommand, and the batch handler always emits JSON.
//! - Strip global key/value flags (`--model VAL`). The daemon runs a single
//!   loaded model; reporting the stripped value is the caller's concern
//!   (see `stripped_model_value`).
//! - Remap `-n <N>` / `--limit <N>` to the canonical `--limit` form the
//!   batch parser expects on the subcommand. Also handles `-n=N` /
//!   `--limit=N` attached-value forms.
//! - Auto-prepend `search` for bare-query invocations (no subcommand, e.g.
//!   `cqs "hello world"`) so the batch parser can route them.
//!
//! Keeping the translation pure (no I/O, no tracing) makes the whole surface
//! unit-testable. The caller owns side effects (warning on stripped
//! `--model`, socket I/O, response parsing).

/// Translate raw CLI argv into a `(subcommand, args)` pair for the batch
/// handler, stripping global flags and normalising `-n`/`--limit`.
///
/// `raw` is the argv after `cqs` (i.e. what `std::env::args().skip(1)` yields).
/// `has_subcommand` is `true` iff `Cli::command` parsed a subcommand — i.e.
/// the first post-strip token is the subcommand name. When `false`, the input
/// is a bare query (e.g. `cqs "find something"`) and `search` is prepended.
///
/// Behaviour is pinned by `tests/daemon_forward_test.rs`. See the module
/// docs for the full stripping/remapping rules.
pub fn translate_cli_args_to_batch(raw: &[String], has_subcommand: bool) -> (String, Vec<String>) {
    // Boolean global flags: drop entirely (no value to track).
    const GLOBAL_FLAGS: &[&str] = &["--json", "-q", "--quiet"];
    // Key/value global flags (space-separated form): drop the flag AND its
    // value. `-n` and `--limit` are intercepted here to rewrite to the
    // canonical `--limit`.
    const GLOBAL_WITH_VALUE: &[&str] = &["--model", "-n", "--limit"];

    let mut args: Vec<String> = Vec::with_capacity(raw.len());
    let mut skip_next = false;
    for arg in raw.iter() {
        if skip_next {
            skip_next = false;
            continue;
        }
        // `--json` / `-q` / `--quiet` → drop.
        if GLOBAL_FLAGS.contains(&arg.as_str()) {
            continue;
        }
        // Attached-value forms (`-n=5`, `--limit=5`, `--model=foo`). Handle
        // here so we don't emit them verbatim and rely on the batch parser
        // to tolerate them. The spaced forms fall through to the next branch.
        if let Some((key, value)) = arg.split_once('=') {
            if key == "-n" || key == "--limit" {
                args.push(format!("--limit={}", value));
                continue;
            }
            if key == "--model" {
                // Strip `--model=VAL` entirely; caller handles the warning.
                continue;
            }
            if GLOBAL_FLAGS.contains(&key) {
                // Edge case: `--json=true` etc. — drop.
                continue;
            }
            // Non-global attached-value flag: pass through verbatim.
        }
        // Spaced-form global key/value flags.
        if GLOBAL_WITH_VALUE.contains(&arg.as_str()) {
            if arg == "-n" || arg == "--limit" {
                // Remap `-n 5` → `--limit 5`. The value is the next token and
                // is forwarded verbatim on the next iteration (skip_next
                // stays false). This mirrors the pre-extraction inline block.
                args.push("--limit".to_string());
                continue;
            }
            // `--model VAL` → strip both tokens. Caller uses
            // `stripped_model_value` to recover VAL for its warning.
            skip_next = true;
            continue;
        }
        args.push(arg.clone());
    }

    if !has_subcommand {
        // Bare query: `cqs "hello world"` → ("search", ["hello world"]).
        return ("search".to_string(), args);
    }
    // With a subcommand: the first surviving token is the subcommand name.
    if let Some((first, rest)) = args.split_first() {
        (first.clone(), rest.to_vec())
    } else {
        // Unreachable in practice: if clap parsed a subcommand the argv
        // contained its name. Defensive empty fallback.
        (String::new(), Vec::new())
    }
}

/// Extract the value of `--model` from the raw argv, if present. Used by the
/// caller to emit a "daemon ignores your --model" warning without duplicating
/// the arg-scanning logic here. Supports both `--model VAL` and `--model=VAL`.
pub fn stripped_model_value(raw: &[String]) -> Option<String> {
    let mut it = raw.iter();
    while let Some(arg) = it.next() {
        if arg == "--model" {
            return it.next().cloned();
        }
        if let Some(rest) = arg.strip_prefix("--model=") {
            return Some(rest.to_string());
        }
    }
    None
}

/// Derive the daemon socket path for a given `cqs_dir`.
///
/// Mirrors `cli::files::daemon_socket_path` exactly. Exposed here so
/// integration tests can compute the path the CLI will try to connect to and
/// bind a mock listener there. The hash is collision-avoidance only (per-
/// project naming) — not a security property; access control relies on the
/// filesystem permissions the real daemon sets (0o600).
#[cfg(unix)]
pub fn daemon_socket_path(cqs_dir: &std::path::Path) -> std::path::PathBuf {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::path::PathBuf;

    let sock_dir = std::env::var("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir());
    let sock_name = format!("cqs-{:x}.sock", {
        let mut h = DefaultHasher::new();
        cqs_dir.hash(&mut h);
        h.finish()
    });
    sock_dir.join(sock_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(tokens: &[&str]) -> Vec<String> {
        tokens.iter().map(|s| s.to_string()).collect()
    }

    // Sanity parity with the inline block that was removed from
    // `try_daemon_query`. The full black-box suite lives in
    // `tests/daemon_forward_test.rs`.

    #[test]
    fn strips_json_bare_query() {
        let (cmd, args) = translate_cli_args_to_batch(&v(&["--json", "search me"]), false);
        assert_eq!(cmd, "search");
        assert_eq!(args, v(&["search me"]));
    }

    #[test]
    fn remaps_dash_n_spaced() {
        let (cmd, args) = translate_cli_args_to_batch(&v(&["impact", "foo", "-n", "5"]), true);
        assert_eq!(cmd, "impact");
        assert_eq!(args, v(&["foo", "--limit", "5"]));
    }

    #[test]
    fn stripped_model_value_spaced_form() {
        assert_eq!(
            stripped_model_value(&v(&["search", "q", "--model", "bge-large"])),
            Some("bge-large".to_string())
        );
    }

    #[test]
    fn stripped_model_value_equals_form() {
        assert_eq!(
            stripped_model_value(&v(&["search", "q", "--model=bge-large"])),
            Some("bge-large".to_string())
        );
    }
}
