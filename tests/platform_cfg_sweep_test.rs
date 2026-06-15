//! Completeness guard for the *platform-cfg divergence* family — the
//! incomplete-sweep class that PR CI is structurally blind to.
//!
//! ## The structural null this defends
//!
//! `.github/workflows/ci.yml` builds, tests, and runs `clippy -D warnings`
//! **only on `ubuntu-latest`**. The cross-target build (Linux + macOS +
//! Windows) lives in `release.yml` and fires only on a `v*` tag. So an entire
//! class of bug — code that compiles clean on Linux but breaks on a *sibling*
//! target in the release matrix {`x86_64-unknown-linux-gnu`,
//! `aarch64-apple-darwin`, `x86_64-pc-windows-msvc`} — is invisible until the
//! release cross-build. v1.46.0's release build is exactly where it last bit.
//!
//! Each Linux build is correct **in isolation** (its own green suite passes);
//! the defect lives in the *relation* across the platform peer-set: a sweep
//! that updated the Linux arm but left a sibling arm diverging or unannotated.
//! No single-target test can express "all three targets type-check", so this
//! guard asserts the *source-level invariants* that keep the sibling arms
//! sound, on the one platform CI actually runs.
//!
//! ## The exemplar (the bug this family is named for)
//!
//! `src/cli/batch/view.rs::resolve_overlay` binds
//! `let ops_root = { match … { #[cfg(target_os="linux")] Some(p) => p.ops_path(), … } }`.
//! On Linux the `ops_path()` arm gives the block a `PathBuf` type. On macOS
//! (`all(unix, not(target_os = "linux"))`) *every* arm diverges (`return` /
//! `warn!`-then-`return`), so the block has no value type and the binding's
//! type is back-inferred from the downstream `&ops_root: &Path` use — yielding
//! the **unsized** `Path`, i.e. `error[E0277]: the size for values of type
//! [u8] cannot be known at compilation time`. The fix is a `: std::path::PathBuf`
//! annotation so the binding has a sized type independent of the (diverging)
//! arms. Confirmed against the compiler with
//! `cargo check --target aarch64-apple-darwin` on a minimal scratch crate.
//!
//! ## What this guard pins
//!
//! 1. A "site moved" guard: the exemplar function still lives where we expect,
//!    so a rename can't silently drop the family member from this enumeration.
//! 2. A forward scan: no `#[cfg(unix)] let X = { … }` block may have a
//!    Linux-only value arm with all-other-arms-diverging unless `X` carries a
//!    type annotation. This is RED on the unfixed exemplar (the bug) and goes
//!    GREEN the moment the `: std::path::PathBuf` annotation is added — the
//!    same red/green bisection a falsifier gives. It then catches the *next*
//!    straggler at PR time on Linux, where it is otherwise invisible until the
//!    tagged release cross-build.
//!
//! Run: `cargo test --test platform_cfg_sweep_test --features cuda-index`

use std::fs;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let p = repo_root().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// (1) Site-moved guard: the exemplar member of the family must still exist at
/// its known location. If `resolve_overlay` is renamed/moved, fix THIS test to
/// track the new home — don't let the family silently lose its named member
/// (which would disarm the forward scan's calibration anchor).
#[test]
fn exemplar_resolve_overlay_site_present() {
    let src = read("src/cli/batch/view.rs");
    assert!(
        src.contains("fn resolve_overlay("),
        "SITE MOVED: src/cli/batch/view.rs no longer defines `resolve_overlay`. \
         The platform-cfg exemplar moved — update tests/platform_cfg_sweep_test.rs \
         to track its new location, or the divergence guard is silently disarmed."
    );
}

/// (2) Forward scan over all of `src/`: any `#[cfg(unix)] let <name> = {` block
/// whose initializer contains a `#[cfg(target_os = "linux")]` value arm must
/// give `<name>` an explicit type annotation.
///
/// Rationale: when the Linux arm supplies the only non-diverging value, the
/// macOS sibling (`all(unix, not(target_os = "linux"))`) has nothing to infer
/// a *sized* type from except a downstream use — the E0277 trap. A type
/// annotation makes the binding sound regardless of which arms diverge.
///
/// Calibration: on current `main` this is RED, naming
/// `src/cli/batch/view.rs::resolve_overlay`'s unannotated `ops_root` — the live
/// straggler. Adding `: std::path::PathBuf` turns it GREEN. That red→green flip
/// on the one-line fix is the proof the binding is the sole offender.
#[test]
fn no_unannotated_linux_only_unix_let_block() {
    let mut offenders: Vec<String> = Vec::new();

    for (rel, text) in collect_src_files() {
        let lines: Vec<&str> = text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            // `#[cfg(unix)]` immediately followed by a `let … = {` initializer.
            if line.trim() != "#[cfg(unix)]" {
                continue;
            }
            let Some(let_line) = lines.get(i + 1) else {
                continue;
            };
            let lt = let_line.trim_start();
            if !lt.starts_with("let ") {
                continue;
            }
            // Only block-initializer `let`s are at risk (the diverge-all match
            // lives inside `{ … }`). `let x = expr;` one-liners can't hit this.
            if !let_line.trim_end().ends_with('{') {
                continue;
            }
            // Already annotated? `let name: Type = {` — sound, skip. A `:`
            // anywhere left of the `=` is a type annotation on the binding.
            let before_eq = let_line.split('=').next().unwrap_or("");
            let annotated = before_eq.contains(':');

            // Walk the initializer block by brace depth; flag if it contains a
            // `#[cfg(target_os = "linux")]` arm (the Linux-only value supplier).
            let mut depth = 0i32;
            let mut has_linux_arm = false;
            let mut started = false;
            let mut j = i + 1;
            'scan: while j < lines.len() {
                for ch in lines[j].chars() {
                    match ch {
                        '{' => {
                            depth += 1;
                            started = true;
                        }
                        '}' => {
                            depth -= 1;
                            if started && depth == 0 {
                                break 'scan;
                            }
                        }
                        _ => {}
                    }
                }
                if lines[j].contains("#[cfg(target_os = \"linux\")]") {
                    has_linux_arm = true;
                }
                j += 1;
            }

            if has_linux_arm && !annotated {
                offenders.push(format!(
                    "{rel}:{} — {} (cfg(unix) block-let with a \
                     #[cfg(target_os=\"linux\")] value arm but NO type annotation)",
                    i + 2,
                    let_line.trim()
                ));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "PLATFORM-CFG STRAGGLER(S) — a `#[cfg(unix)] let X = {{ … }}` block has a \
         Linux-only value arm but no type annotation. On macOS \
         (`all(unix, not(target_os=\"linux\"))`) the non-Linux arms diverge and X \
         back-infers an unsized type from its downstream use → E0277 on the release \
         cross-build, invisible to Linux-only PR CI. Add a `: <SizedType>` \
         annotation (see src/cli/batch/view.rs::resolve_overlay). Offenders:\n  {}",
        offenders.join("\n  ")
    );
}

/// Recursively collect `(relative_path, contents)` for every `.rs` under `src/`.
fn collect_src_files() -> Vec<(String, String)> {
    let root = repo_root();
    let src = root.join("src");
    let mut out = Vec::new();
    let mut stack = vec![src];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().and_then(|e| e.to_str()) == Some("rs") {
                let rel = p
                    .strip_prefix(&root)
                    .unwrap_or(&p)
                    .to_string_lossy()
                    .replace('\\', "/");
                if let Ok(text) = fs::read_to_string(&p) {
                    out.push((rel, text));
                }
            }
        }
    }
    out
}
