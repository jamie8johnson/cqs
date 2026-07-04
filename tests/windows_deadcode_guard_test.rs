//! RED/GREEN verification for `scripts/check_windows_deadcode.py`, the ledgered
//! guard for the dead-code-on-a-sibling-target (Windows) regression class.
//!
//! ## Why the logic is tested here, on Linux
//!
//! The guard's INPUT — a Windows-target `cargo check --message-format=json`
//! dead_code set — can only be produced on a `windows-latest` runner (the
//! set is cfg-driven and needs the MSVC toolchain; a cross `cargo check` from
//! Linux dies in the C-dep build scripts before `Compiling cqs`). So the guard
//! itself runs as a step in the `windows-check` CI job, not as a `cargo test`.
//!
//! What CAN be verified anywhere is the guard's DECISION LOGIC: given a set of
//! dead_code diagnostics and a ledger, does it fail on a NEW straggler, pass
//! when the ledger matches, fail on a STALE entry (anti-stale exactness), and
//! pass a hard error through? This test drives the real script with SYNTHETIC
//! cargo-JSON fixtures over a scratch ledger and pins each of those branches —
//! the same red/green bisection a Windows run would give, minus the toolchain.
//! It runs under the Linux CI `test` job and locally.
//!
//! Run: `cargo test --features cuda-index --test windows_deadcode_guard_test`

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script() -> PathBuf {
    manifest_dir().join("scripts/check_windows_deadcode.py")
}

/// Per-test-binary scratch dir cargo provides; no external tempfile dep.
fn scratch() -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
}

/// A single `compiler-message` JSON line as cargo emits under
/// `--message-format=json`. `code` is the lint code (e.g. `dead_code`); pass
/// `""` for an error diagnostic (no code).
fn diag(level: &str, code: &str, message: &str, file: &str) -> String {
    let code_field = if code.is_empty() {
        "null".to_string()
    } else {
        format!(r#"{{"code":"{code}"}}"#)
    };
    format!(
        r#"{{"reason":"compiler-message","message":{{"level":"{level}","code":{code_field},"message":"{message}","rendered":"{level}: {message}\n","spans":[{{"file_name":"{file}","line_start":10,"is_primary":true}}]}}}}"#
    )
}

/// Write `contents` to `scratch()/name` and return the path.
fn write(name: &str, contents: &str) -> PathBuf {
    let p = scratch().join(name);
    std::fs::write(&p, contents).unwrap_or_else(|e| panic!("write {}: {e}", p.display()));
    p
}

/// Run the guard with `--ledger LEDGER` over `cargo_json`, plus `extra` args.
fn run_guard(ledger: &Path, cargo_json: &Path, extra: &[&str]) -> Output {
    let mut cmd = Command::new("python3");
    cmd.arg(script())
        .arg("--ledger")
        .arg(ledger)
        .args(extra)
        .arg(cargo_json);
    cmd.output().unwrap_or_else(|e| {
        panic!(
            "spawn python3 for {}: {e} (python3 is a hard dependency of the \
             windows-deadcode guard; it is present in CI and dev shells)",
            script().display()
        )
    })
}

/// Two dead_code items in `src/` plus one dependency warning at an absolute
/// registry path (which the guard must ignore — it is not cqs's own source).
fn fixture_two_items() -> String {
    [
        diag(
            "warning",
            "dead_code",
            "function `format_uptime` is never used",
            "src/cli/watch/runtime.rs",
        ),
        diag(
            "warning",
            "dead_code",
            "struct `DaemonHint` is never constructed",
            "src/cli/batch/view.rs",
        ),
        diag(
            "warning",
            "dead_code",
            "function `dep_thing` is never used",
            "/home/u/.cargo/registry/src/x/lib.rs",
        ),
        r#"{"reason":"build-finished","success":true}"#.to_string(),
    ]
    .join("\n")
        + "\n"
}

const LEDGER_BOTH: &str = "src/cli/watch/runtime.rs\tfunction `format_uptime` is never used\n\
     src/cli/batch/view.rs\tstruct `DaemonHint` is never constructed\n";

/// RED: an item absent from the ledger is a NEW straggler → exit 1, named.
/// (An empty ledger makes every item NEW, which is also the bootstrap-seed
/// path, so this one case pins both regression-detection and first-seed.)
#[test]
fn new_straggler_fails_and_is_named() {
    let ledger = write("ledger_empty.txt", "# header only, zero entries\n");
    let json = write("check_new.json", &fixture_two_items());
    let out = run_guard(&ledger, &json, &[]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "guard must FAIL on a dead_code item absent from the ledger; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("NEW dead_code")
            && stderr.contains("format_uptime")
            && stderr.contains("DaemonHint"),
        "failure must name the new stragglers; stderr:\n{stderr}"
    );
    // The dependency warning (absolute registry path) must NOT be treated as a
    // ledgerable item: its path never appears in the NEW list or the ledger
    // block. (Its message may show in the rendered-diagnostics dump the guard
    // prints for CI-log readability; that is the render, not the item report —
    // so key the assertion on the registry PATH, which only surfaces if the
    // guard wrongly scoped it in.)
    assert!(
        !stderr.contains("/home/u/.cargo/registry"),
        "a dependency warning (absolute registry path) must be out of scope; stderr:\n{stderr}"
    );
    // A copy-paste ledger block is emitted so seeding/re-blessing is mechanical.
    assert!(
        stderr.contains("===WINDOWS-DEADCODE-LEDGER-BEGIN===")
            && stderr.contains("===WINDOWS-DEADCODE-LEDGER-END==="),
        "failure must print the copy-paste ledger block; stderr:\n{stderr}"
    );
}

/// GREEN: a ledger that matches the diagnostics exactly → exit 0.
#[test]
fn matching_ledger_passes() {
    let ledger = write("ledger_both.txt", LEDGER_BOTH);
    let json = write("check_match.json", &fixture_two_items());
    let out = run_guard(&ledger, &json, &[]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "guard must PASS when the ledger matches the dead_code set; stderr:\n{stderr}"
    );
}

/// Anti-stale exactness: a ledger entry with no matching diagnostic → exit 1,
/// demanding its removal. This is the floor that stops a cleared backlog item
/// from lingering as dead weight (mirrors the JSON-noise guard's stale check).
#[test]
fn stale_ledger_entry_fails_and_demands_removal() {
    let ledger = write(
        "ledger_stale.txt",
        &format!("{LEDGER_BOTH}src/gone.rs\tfunction `removed` is never used\n"),
    );
    let json = write("check_stale.json", &fixture_two_items());
    let out = run_guard(&ledger, &json, &[]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "guard must FAIL on a stale ledger entry; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("STALE ledger entry") && stderr.contains("removed"),
        "failure must name the stale entry to remove; stderr:\n{stderr}"
    );
}

/// The hard-error gate: a non-zero cargo exit is the E0432/E0277 class the
/// windows-check job exists to catch → exit 1, without consulting the ledger.
#[test]
fn hard_error_cargo_rc_fails() {
    let ledger = write("ledger_hard.txt", LEDGER_BOTH);
    // An error-level diagnostic + non-zero cargo rc (as a real hard error emits).
    let json = write(
        "check_hard.json",
        &(diag(
            "error",
            "",
            "cannot find type `X` in this scope",
            "src/serve/daemon_client.rs",
        ) + "\n"),
    );
    let out = run_guard(&ledger, &json, &["--cargo-rc", "1"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "guard must FAIL when cargo check reported a hard error; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("error(s)") && stderr.contains("Windows"),
        "hard-error failure must be explained; stderr:\n{stderr}"
    );
}

/// The committed ledger must be well-formed: every non-comment, non-blank line
/// carries the `<path>\t<message>` TAB separator the guard parses. Guards the
/// checked-in `tests/windows_deadcode_ledger.txt` directly, independent of any
/// diagnostics, so a hand-corruption fails loudly at PR time.
#[test]
fn committed_ledger_is_well_formed() {
    let path = manifest_dir().join("tests/windows_deadcode_ledger.txt");
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    for (i, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        assert!(
            line.contains('\t'),
            "malformed ledger entry at {}:{} (no TAB separator): {line:?}",
            path.display(),
            i + 1,
        );
    }
}
