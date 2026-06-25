//! CLI-argv to batch-request translation and daemon-socket path helpers.
//!
//! Pure arg-shaping logic, kept separate from `cli::dispatch::try_daemon_query`
//! so integration tests (`tests/daemon_forward_test.rs`) can exercise it
//! without reaching into the binary-only `cli` module tree.
//!
//! Also exposes `daemon_socket_path` (the real function used by the CLI) so
//! tests can compute the exact socket path for a given `cqs_dir` and bind a
//! mock `UnixListener` there.
//!
//! The daemon speaks the same syntax as `cqs batch`: one JSON object per line
//! with `{"command": "<sub>", "args": [...]}`. The CLI args that reach
//! `try_daemon_query` are the raw argv post-`cqs` (i.e. `std::env::args()
//! .skip(1)`) — still mixed with top-level flags the `Cli` struct consumes
//! (`--json`, `-v`, `--model VAL`, `--rrf`, …) and sub-command flags that the
//! batch parser consumes on the subcommand side.
//!
//! Responsibilities:
//!
//! - **With a subcommand**: drop every top-level token before the subcommand
//!   name (those flags configure the CLI process — output shape, logging,
//!   model selection — and do not reach subcommand handlers on the in-process
//!   path either), then forward everything after the subcommand verbatim.
//!   The CLI and batch sides flatten the *same* shared `args::*Args` structs,
//!   so post-subcommand tokens parse identically on both surfaces — including
//!   per-command short flags whose meaning differs from the top level (e.g.
//!   blame's `-n` means `--commits`, not `--limit`).
//! - **Bare query** (`cqs "find something"`): prepend `search` and translate
//!   the top-level flags for the batch `search` parser. Search knobs
//!   (`--rrf`, `-t`, `-n`, `--tokens`, …) mirror `args::SearchArgs` spelling
//!   for spelling and forward verbatim; process-local flags (`--json`, `-q`,
//!   `-v`, `--model`, `--slot`) are dropped.
//!
//! Which top-level flags exist, which take values, and which are
//! process-local is not hardcoded here: the caller derives a [`CliArgSpec`]
//! from the live clap definition ([`CliArgSpec::from_clap`]) so a new
//! top-level flag is classified automatically instead of hand-mirrored —
//! hand-mirroring is how `-v <cmd>` / `--rrf <cmd>` came to hard-error
//! daemon-up while working daemon-down.
//!
//! Keeping the translation pure (no I/O, no tracing, no clap statics) makes
//! the whole surface unit-testable. The caller owns side effects (warning on
//! stripped `--model`, socket I/O, response parsing).

/// Classification of the top-level CLI flag surface, consumed by
/// [`translate_cli_args_to_batch`]. Plain data — the helper stays pure and
/// testable without a clap `Command` in hand.
///
/// Built from the live clap definition via [`CliArgSpec::from_clap`] in
/// production (see `cli::dispatch`), or by hand in tests.
#[derive(Debug, Clone, Default)]
pub struct CliArgSpec {
    /// Every spelling (`--long`, `--alias`, `-s`) of a top-level flag that
    /// consumes a following value token. Needed to skip `--flag value` pairs
    /// when scanning for the subcommand name, and to forward the value token
    /// verbatim on the bare-query path.
    pub value_flags: std::collections::BTreeSet<String>,
    /// Spellings of top-level flags that are process-local (output shaping,
    /// logging, model/slot selection) and must be dropped when forwarding a
    /// bare query to the batch `search` parser. Every other top-level flag is
    /// a search knob shared spelling-for-spelling with `args::SearchArgs` and
    /// forwards verbatim.
    pub bare_query_strip: std::collections::BTreeSet<String>,
    /// Every spelling (`--lang`/`-l`, `--path`/`-p`, …) of a top-level
    /// *scope* flag — a filter the top-level `Cli` consumes that also exists,
    /// spelling-for-spelling, on at least one subcommand's batch surface.
    /// These reach a CLI subcommand through the top-level region
    /// (`cqs --lang rust similar foo`) and so are re-forwarded onto the
    /// subcommand tail rather than dropped with the rest of that region.
    /// Derived from the live clap definitions by [`CliArgSpec::from_clap`];
    /// the per-subcommand decision lives in [`Self::scope_targets`].
    pub scope_flags: std::collections::BTreeSet<String>,
    /// Subcommand names whose batch `*Args` surface accepts at least one
    /// [`Self::scope_flags`] spelling. Top-level scope flags are forwarded
    /// onto exactly these subcommands' tails; every other subcommand drops
    /// the whole top-level region. Derived from the batch clap definition,
    /// not hand-listed — adding `lang`/`path` to another wire `*Args` struct
    /// extends this set automatically.
    pub scope_targets: std::collections::BTreeSet<String>,
}

impl CliArgSpec {
    /// Derive the spec from a clap `Command` (the top-level `Cli`
    /// definition). `process_local_ids` lists the clap arg IDs (struct field
    /// names) of flags that configure the CLI process rather than the search:
    /// those are stripped on the bare-query path; everything else forwards.
    ///
    /// Mirrors the runtime-derivation pattern in
    /// `cli::telemetry::describe_command`: value-flag-ness comes from the arg
    /// action, spellings include long/short forms and all aliases, so a new
    /// top-level flag is picked up without touching this module. A
    /// classification test on the caller side pins that every top-level arg
    /// ID is explicitly process-local or search-forwarded.
    ///
    /// `scope_ids` lists the top-level arg IDs (struct field names) whose
    /// value is a *search scope* the subcommand handler honors when the same
    /// flag is present on its own batch surface (`lang`, `path`). Their
    /// spellings are collected into [`Self::scope_flags`]; the batch clap
    /// definition (`batch_cmd`) is probed per *daemon-capable* subcommand
    /// (`daemon_capable_subcommands`) to populate [`Self::scope_targets`]
    /// with exactly the subcommands whose `*Args` accept at least one of those
    /// spellings — so forwarding tracks the wire structs, not a hand-maintained
    /// list. The daemon-capability filter keeps a CLI-only subcommand that
    /// happens to carry `lang` (e.g. batch `diff`/`drift`) out of the set: those
    /// never reach the daemon translator, so listing them would be inert noise.
    pub fn from_clap(
        cmd: &clap::Command,
        process_local_ids: &[&str],
        batch_cmd: &clap::Command,
        scope_ids: &[&str],
        daemon_capable_subcommands: &[&str],
    ) -> Self {
        let mut value_flags = std::collections::BTreeSet::new();
        let mut bare_query_strip = std::collections::BTreeSet::new();
        let mut scope_flags = std::collections::BTreeSet::new();
        for arg in cmd.get_arguments() {
            if arg.is_positional() {
                continue;
            }
            let takes_value = matches!(
                arg.get_action(),
                clap::ArgAction::Set | clap::ArgAction::Append
            );
            let spellings = arg_spellings(arg);
            let is_local = process_local_ids.contains(&arg.get_id().as_str());
            let is_scope = scope_ids.contains(&arg.get_id().as_str());
            for spelling in spellings {
                if takes_value {
                    value_flags.insert(spelling.clone());
                }
                if is_local {
                    bare_query_strip.insert(spelling.clone());
                }
                if is_scope {
                    scope_flags.insert(spelling);
                }
            }
        }
        let scope_targets =
            derive_scope_targets(batch_cmd, &scope_flags, daemon_capable_subcommands);
        Self {
            value_flags,
            bare_query_strip,
            scope_flags,
            scope_targets,
        }
    }
}

/// Every spelling of a clap arg: `--long`, every `--alias`, `-s`, every
/// `-a` short alias. Shared by the top-level and per-subcommand scans so
/// both derive flag spellings the same way.
fn arg_spellings(arg: &clap::Arg) -> Vec<String> {
    let mut spellings: Vec<String> = Vec::new();
    if let Some(long) = arg.get_long() {
        spellings.push(format!("--{long}"));
    }
    for alias in arg.get_all_aliases().unwrap_or_default() {
        spellings.push(format!("--{alias}"));
    }
    if let Some(short) = arg.get_short() {
        spellings.push(format!("-{short}"));
    }
    for alias in arg.get_all_short_aliases().unwrap_or_default() {
        spellings.push(format!("-{alias}"));
    }
    spellings
}

/// Walk every daemon-capable batch subcommand and collect the names whose
/// flattened `*Args` surface accepts at least one `scope_flags` spelling.
/// This is the derivation that replaces the old `cmd == "similar"` string
/// match: a subcommand is a forward target iff its batch clap definition
/// actually carries the scope flag, so adding `lang`/`path` to another
/// daemon-routed wire `*Args` struct enrolls that subcommand automatically.
///
/// `daemon_capable` gates the walk to subcommands the daemon translator can
/// actually receive — a CLI-only batch subcommand that carries `lang` (batch
/// `diff`/`drift`) never reaches this code path, so it's excluded rather than
/// listed as an inert target.
fn derive_scope_targets(
    batch_cmd: &clap::Command,
    scope_flags: &std::collections::BTreeSet<String>,
    daemon_capable: &[&str],
) -> std::collections::BTreeSet<String> {
    let mut targets = std::collections::BTreeSet::new();
    for sub in batch_cmd.get_subcommands() {
        let name = sub.get_name();
        if !daemon_capable.contains(&name) {
            continue;
        }
        let accepts_scope = sub
            .get_arguments()
            .filter(|a| !a.is_positional())
            .flat_map(arg_spellings)
            .any(|s| scope_flags.contains(&s));
        if accepts_scope {
            targets.insert(name.to_string());
        }
    }
    targets
}

/// Translate raw CLI argv into a `(subcommand, args)` pair for the batch
/// handler.
///
/// `raw` is the argv after `cqs` (i.e. what `std::env::args().skip(1)`
/// yields). `has_subcommand` is `true` iff `Cli::command` parsed a
/// subcommand; when `false` the input is a bare query (e.g. `cqs "find
/// something"`) and `search` is prepended. `spec` classifies the top-level
/// flag surface — see [`CliArgSpec`].
///
/// Behaviour is pinned by `tests/daemon_forward_test.rs`. See the module
/// docs for the full rules.
pub fn translate_cli_args_to_batch(
    raw: &[String],
    has_subcommand: bool,
    spec: &CliArgSpec,
) -> (String, Vec<String>) {
    if has_subcommand {
        translate_subcommand_args(raw, spec)
    } else {
        ("search".to_string(), translate_bare_query_args(raw, spec))
    }
}

/// Subcommand form: drop the top-level region (every flag before the
/// subcommand name), forward the tail verbatim. The batch parser flattens
/// the same shared args structs as the CLI subcommand, so no per-flag
/// rewriting is needed — and none is safe, because short-flag meanings
/// differ per subcommand (blame's `-n` is `--commits`).
fn translate_subcommand_args(raw: &[String], spec: &CliArgSpec) -> (String, Vec<String>) {
    let mut idx = 0;
    // Top-level scope flags (`--lang`/`-l`, `--path`/`-p`) reach a CLI
    // subcommand handler through the top-level `Cli` region (`cqs --lang rust
    // similar foo`); they're dropped with the rest of that region below.
    // Capture them here so they can be re-forwarded onto the subcommand tail
    // for subcommands whose batch `*Args` surface accepts them — keeping
    // daemon-routed scoping on par with the CLI-direct path. Which subcommands
    // those are is derived from the batch clap definition (`spec.scope_targets`),
    // not hand-listed: a subcommand enrolls automatically when its wire
    // `*Args` struct carries the scope flags.
    let mut scope_forward: Vec<String> = Vec::new();
    while idx < raw.len() {
        let tok = &raw[idx];
        if !tok.starts_with('-') {
            // First non-flag token in the top-level region: the subcommand.
            break;
        }
        // `--key=value` / `-k=value` are self-contained single tokens;
        // spaced-form value flags consume their following value token too;
        // boolean flags consume only themselves.
        if !tok.contains('=') && spec.value_flags.contains(tok.as_str()) {
            if spec.scope_flags.contains(tok.as_str()) {
                // Spaced form: capture the flag and its following value token.
                if let Some(val) = raw.get(idx + 1) {
                    scope_forward.push(tok.clone());
                    scope_forward.push(val.clone());
                }
            }
            idx += 2;
        } else {
            if let Some((key, _)) = tok.split_once('=') {
                // Attached form (`--lang=rust`): forward the whole token.
                if spec.scope_flags.contains(key) {
                    scope_forward.push(tok.clone());
                }
            }
            idx += 1;
        }
    }
    let Some(cmd) = raw.get(idx).cloned() else {
        // Unreachable in practice: if clap parsed a subcommand the argv
        // contained its name. Defensive empty fallback.
        return (String::new(), Vec::new());
    };
    let mut tail: Vec<String> = raw[idx + 1..].to_vec();
    if spec.scope_targets.contains(&cmd) && !scope_forward.is_empty() {
        // Splice the captured top-level scope flags in *front* of the tail so
        // the daemon dispatch builds the same `--lang`/`--path` filter the CLI
        // would. A subcommand tail can also carry these directly (`cqs similar
        // foo --lang rust`); putting the top-level capture first means clap's
        // last-wins resolution keeps any tail-explicit value authoritative.
        scope_forward.extend(tail);
        tail = scope_forward;
    }
    // `cqs notes list ...` → daemon `notes ...`. The CLI's `Notes`
    // command takes a NotesCommand subcommand enum (`list`/`add`/
    // `update`/`remove`) but the batch dispatcher only ever receives
    // `list` — `add`/`update`/`remove` route through `BatchSupport::Cli`
    // and never hit the daemon. The batch parser's `BatchCmd::Notes`
    // accepts `--warnings`/`--patterns` directly (no `list` token), so
    // we strip the redundant subcommand name here. Without this strip
    // every `cqs notes list` query through the daemon errors with
    // `unexpected argument 'list' found`.
    if cmd == "notes" && tail.first().map(|s| s.as_str()) == Some("list") {
        tail.remove(0);
    }
    (cmd, tail)
}

/// Whether `tok` is a combined-short cluster (`-qv`, `-qn5`): a single `-`
/// followed by two or more characters, none of them `-` (so `--long` and
/// `-x=val` are excluded — those are handled by the long / attached-value
/// paths). Clap accepts these clusters when the daemon is down, so they reach
/// the bare-query forward path and must be expanded before classification.
fn is_combined_short(tok: &str) -> bool {
    let Some(rest) = tok.strip_prefix('-') else {
        return false;
    };
    rest.len() >= 2 && !rest.starts_with('-') && !rest.contains('=')
}

/// Expand a combined-short cluster into individual short tokens the bare-query
/// loop understands. Mirrors clap's cluster semantics: scan left to right,
/// each character is a short flag `-x`; the first value-taking short consumes
/// the remainder of the cluster as its attached value (emitted as `-x=rest`)
/// and ends the cluster, or takes the next argv token via the spaced-form path
/// when nothing follows it in the cluster. A character not known to the spec is
/// treated as a trailing value for the preceding value flag if one is open,
/// otherwise emitted as a bare short so the batch parser owns the rejection.
fn expand_combined_short(cluster: &str, spec: &CliArgSpec) -> Vec<String> {
    let mut out = Vec::new();
    let chars: Vec<char> = cluster.chars().skip(1).collect();
    let mut i = 0;
    while i < chars.len() {
        let short = format!("-{}", chars[i]);
        let rest: String = chars[i + 1..].iter().collect();
        if spec.value_flags.contains(&short) {
            if rest.is_empty() {
                // No attached value in the cluster: emit the bare flag and let
                // the main loop consume the next argv token (spaced form).
                out.push(short);
            } else {
                // Remainder is the attached value (`-n5` → `-n=5`).
                out.push(format!("{short}={rest}"));
            }
            return out;
        }
        out.push(short);
        i += 1;
    }
    out
}

/// Bare-query form: every token is top-level. Process-local flags
/// (`spec.bare_query_strip`) are dropped — with their value, when they take
/// one; everything else forwards verbatim to the batch `search` parser,
/// whose `args::SearchArgs` mirrors the top-level search knobs spelling for
/// spelling (including `-n`/`--limit` via the shared `LimitArg`).
fn translate_bare_query_args(raw: &[String], spec: &CliArgSpec) -> Vec<String> {
    // Pre-expand combined-short clusters (`-qv` → `-q -v`) so each short is
    // classified individually; clap accepts clusters daemon-down but the batch
    // parser rejects them, which would be a non-recoverable protocol error.
    let mut expanded: Vec<String> = Vec::with_capacity(raw.len());
    for tok in raw {
        if is_combined_short(tok) {
            expanded.extend(expand_combined_short(tok, spec));
        } else {
            expanded.push(tok.clone());
        }
    }

    let mut out: Vec<String> = Vec::with_capacity(expanded.len());
    let mut iter = expanded.iter();
    while let Some(tok) = iter.next() {
        // Attached-value forms (`--model=foo`, `--json=true`, `-n=5`):
        // self-contained tokens — drop or forward whole.
        if let Some((key, _value)) = tok.split_once('=') {
            if key.starts_with('-') && spec.bare_query_strip.contains(key) {
                continue;
            }
            out.push(tok.clone());
            continue;
        }
        if spec.bare_query_strip.contains(tok.as_str()) {
            if spec.value_flags.contains(tok.as_str()) {
                // Spaced form: drop the flag's value token too.
                iter.next();
            }
            continue;
        }
        if tok.starts_with('-') && spec.value_flags.contains(tok.as_str()) {
            // Forwarded value flag: emit the flag and its value verbatim so
            // a value that happens to start with `-` isn't re-scanned as a
            // flag.
            out.push(tok.clone());
            if let Some(value) = iter.next() {
                out.push(value.clone());
            }
            continue;
        }
        out.push(tok.clone());
    }
    out
}

/// Resolve the daemon socket read/write timeout used on both the CLI client
/// side (`cli::dispatch::try_daemon_query`) and the daemon server side
/// (`cli::watch::handle_socket_client`).
///
/// A single env knob keeps the two surfaces symmetric: if the CLI cap is
/// raised to allow a slow rerank but the daemon's write cap stays low, the
/// client gets a truncated/parse-error JSON line.
///
/// Honors `CQS_DAEMON_TIMEOUT_MS` (millisecond integer). Defaults to
/// 30 s; floors to 1 s so a misconfigured `=500` doesn't collapse to
/// `Duration::from_secs(0)` (which is unusable on UnixStream timeouts).
pub fn resolve_daemon_timeout_ms() -> std::time::Duration {
    std::time::Duration::from_millis(
        std::env::var("CQS_DAEMON_TIMEOUT_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .map(|ms| ms.max(1_000))
            .unwrap_or(30_000),
    )
}

/// Extract the value of `--model` from the raw argv, if present. Used by the
/// caller to emit a "daemon ignores your --model" warning without duplicating
/// the arg-scanning logic here. Supports both `--model VAL` and `--model=VAL`.
/// Classification of a daemon dispatch output against the slim batch
/// envelope contract (`wrap_value` / `wrap_error` in `json_envelope`):
/// `{"data": <payload>}` or `{"error": {"code","message"}}`, each with an
/// optional `_meta` sibling and NO other keys. Full v1 envelopes (which
/// carry `version`) and payloads that merely contain a `data` field among
/// other keys deliberately do not match — they pass through verbatim.
pub enum SlimEnvelope<'a> {
    /// Success: the inner payload plus the daemon's `_meta`, if any.
    Data {
        payload: &'a serde_json::Value,
        meta: Option<&'a serde_json::Value>,
    },
    /// Failure: redacted code + message from the slim error object.
    Error { code: String, message: String },
}

/// Match `v` against the slim envelope shapes. Returns `None` for anything
/// that isn't exactly a slim envelope, so callers print unrecognized output
/// unchanged.
pub fn classify_slim_envelope(v: &serde_json::Value) -> Option<SlimEnvelope<'_>> {
    let obj = v.as_object()?;
    if obj.is_empty()
        || !obj
            .keys()
            .all(|k| k == "data" || k == "error" || k == "_meta")
    {
        return None;
    }
    let meta = obj.get("_meta");
    match (obj.get("data"), obj.get("error")) {
        (Some(payload), None) => Some(SlimEnvelope::Data { payload, meta }),
        (None, Some(err)) => Some(SlimEnvelope::Error {
            code: err
                .get("code")
                .and_then(|c| c.as_str())
                .unwrap_or("internal")
                .to_string(),
            message: err
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("daemon error")
                .to_string(),
        }),
        _ => None,
    }
}

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
///
/// Hashes via [`blake3`] rather than `DefaultHasher`: `DefaultHasher` is
/// Rust-version-dependent SipHash, so a `cargo update` of std could change
/// socket names and break systemd `cqs-watch` units that hardcode a path.
/// BLAKE3 is stable across Rust versions; truncating the digest to 8 bytes
/// keeps the socket name short while staying collision-safe (~1e-15 for 100
/// projects).
#[cfg(unix)]
pub fn daemon_socket_path(cqs_dir: &std::path::Path) -> std::path::PathBuf {
    use std::path::PathBuf;

    // Thread-local override (test-only) takes precedence so test fixtures
    // can redirect the socket without `unsafe std::env::set_var`. None in
    // production. See SOCKET_DIR_OVERRIDE.
    let override_dir =
        SOCKET_DIR_OVERRIDE.with(|cell| cell.borrow().as_ref().map(|p| p.to_path_buf()));

    let sock_dir = match override_dir {
        Some(d) => d,
        None => match std::env::var_os("XDG_RUNTIME_DIR") {
            Some(d) => PathBuf::from(d),
            None => {
                // Fallback to /tmp (mode 1777) is fine on macOS
                // (`/var/folders/.../T/` is per-user) but a trust drop on
                // multi-user Linux. Surface it once so operators can wire
                // `XDG_RUNTIME_DIR=/run/user/$(id -u)` if they care.
                //
                // Nest the socket inside a per-uid private subdirectory
                // (`temp_dir()/cqs-<uid>/`) so the `remove_file` → `bind`
                // TOCTOU window is contained to a 0o700 dir only the current
                // uid can write to. The dir is created with the 0o700 mode by
                // `ensure_socket_parent_dir` at daemon startup; this fn just
                // builds the path.
                #[cfg(target_os = "linux")]
                {
                    static WARNED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
                    WARNED.get_or_init(|| {
                        tracing::info!(
                            "XDG_RUNTIME_DIR unset — daemon socket falls back to temp_dir/cqs-<uid>/; \
                             consider XDG_RUNTIME_DIR=/run/user/$(id -u)"
                        );
                    });
                }
                #[cfg(unix)]
                {
                    let uid = unsafe { libc::getuid() };
                    std::env::temp_dir().join(format!("cqs-{}", uid))
                }
                #[cfg(not(unix))]
                {
                    std::env::temp_dir()
                }
            }
        },
    };
    // BLAKE3 is stable across Rust versions — important because systemd unit
    // files and operator scripts encode the socket path. Truncate to 8 hex
    // bytes (16 chars) — collision probability for 100 projects is ~1e-15.
    let canonical_path_bytes = cqs_dir.as_os_str().as_encoded_bytes();
    let hash = blake3::hash(canonical_path_bytes);
    let truncated = &hash.as_bytes()[..8];
    let mut hex = String::with_capacity(16);
    for b in truncated {
        use std::fmt::Write as _;
        let _ = write!(hex, "{:02x}", b);
    }
    let sock_name = format!("cqs-{}.sock", hex);
    sock_dir.join(sock_name)
}

/// Ensure the socket's parent directory exists with `0o700` so the
/// cleanup-then-bind TOCTOU window is contained.
///
/// Called from the daemon startup path (`cli/watch/mod.rs`) BEFORE the
/// stale-socket cleanup + `UnixListener::bind` sequence runs. With this
/// guarantee, a hostile local user can't plant their own socket at the
/// daemon's bind path during the gap between `remove_file` and `bind`,
/// because they don't have write access to the parent directory.
///
/// On `XDG_RUNTIME_DIR` paths (`/run/user/<uid>`) the parent is already
/// `0o700` per systemd's contract — this function still verifies the
/// path exists and is a directory, but skips the chmod. On the `/tmp`
/// fallback path the parent is `temp_dir()/cqs-<uid>/` which we own;
/// create it with `0o700` if missing, or fail loudly if a file/symlink
/// is squatting there.
///
/// Production-only: tests using [`set_socket_dir_override_for_test`] set
/// up their own per-test tempdir and skip this check (the test's tempdir
/// is `0o700` by default on most platforms).
#[cfg(unix)]
pub fn ensure_socket_parent_dir(socket_path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let parent = match socket_path.parent() {
        Some(p) => p,
        None => return Ok(()), // bare filename — no parent to secure
    };
    match std::fs::symlink_metadata(parent) {
        Ok(md) => {
            if md.file_type().is_symlink() {
                return Err(std::io::Error::other(format!(
                    "SEC-V1.36-10: refusing to use socket parent dir {} \
                     because it is a symlink (potential trust escalation)",
                    parent.display()
                )));
            }
            if !md.is_dir() {
                return Err(std::io::Error::other(format!(
                    "SEC-V1.36-10: socket parent path {} exists but is not a directory \
                     (regular file or other inode squatting on the path)",
                    parent.display()
                )));
            }
            // Existing dir — verify mode is private. systemd-managed
            // `/run/user/<uid>/` is 0o700; tempdir-fallback `/tmp/cqs-<uid>/`
            // we created earlier is also 0o700; an unexpected loose mode
            // on either is a real anomaly.
            let mode = md.permissions().mode() & 0o777;
            if mode & 0o077 != 0 {
                tracing::warn!(
                    parent = %parent.display(),
                    mode = format!("{:o}", mode),
                    "SEC-V1.36-10: socket parent dir mode is not 0o700; tightening"
                );
                std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Create the per-uid socket dir with private mode. We can't
            // pass mode to `create_dir`, so set umask 0o077 around it as
            // belt-and-suspenders, then chmod explicitly.
            let prev_umask = unsafe { libc::umask(0o077) };
            let create_result = std::fs::create_dir_all(parent);
            unsafe { libc::umask(prev_umask) };
            create_result?;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
            tracing::debug!(
                parent = %parent.display(),
                "SEC-V1.36-10: created private socket parent dir (0o700)"
            );
        }
        Err(e) => return Err(e),
    }
    Ok(())
}

// Thread-local override for the socket directory. Production paths leave
// this `None` and `daemon_socket_path` reads `XDG_RUNTIME_DIR`; tests set it
// via `set_socket_dir_override_for_test` to redirect the socket under a
// per-test tempdir without touching process-wide env vars.
//
// Thread-local rather than `unsafe std::env::set_var` because setenv races
// with concurrent env reads from any other thread, deadlocking libc's env
// mutex on parallel test runners. A thread-local override has no cross-thread
// state and lets the daemon round-trip tests run fully parallel.
#[cfg(unix)]
std::thread_local! {
    static SOCKET_DIR_OVERRIDE: std::cell::RefCell<Option<std::path::PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// Test-only hook to redirect the daemon socket directory on the current
/// thread. Pass `None` to clear. See [`SOCKET_DIR_OVERRIDE`] for the
/// rationale.
///
/// `#[doc(hidden)]` because it's strictly a test utility — the symbol is
/// `pub` so integration tests can reach it but it's not part of the
/// public API.
#[cfg(unix)]
#[doc(hidden)]
pub fn set_socket_dir_override_for_test(dir: Option<std::path::PathBuf>) {
    SOCKET_DIR_OVERRIDE.with(|cell| *cell.borrow_mut() = dir);
}

/// Typed error returned by [`daemon_ping`], [`daemon_status`], and
/// [`daemon_reconcile`].
///
/// Lets callers distinguish "daemon never ran" (socket file missing) from
/// "daemon crashed mid-call" (transport failure) from "daemon answered
/// with garbage" (envelope/JSON parse failure) from "daemon answered
/// with `status: \"err\"`" (handler-level error). The `wait_for_fresh`
/// hot path branches on the variant to produce different bail messages
/// per failure mode rather than collapsing every failure into "no
/// daemon — start one" advice.
#[cfg(unix)]
#[derive(Debug, Clone, thiserror::Error)]
pub enum DaemonRpcError {
    /// The daemon socket file does not exist — the daemon never started
    /// (or was stopped). Operator action: start `cqs watch --serve`.
    #[error("daemon socket missing: {0}")]
    SocketMissing(String),
    /// Connect / read / write / timeout / set_*_timeout failure. The
    /// socket file exists but the daemon isn't responding. Operator
    /// action: check `journalctl --user -u cqs-watch` and consider
    /// restarting the unit.
    #[error("daemon transport failure: {0}")]
    Transport(String),
    /// Envelope JSON parse, missing/non-string `status` field, or the
    /// dispatch payload deserialize failed. The daemon answered but the
    /// response is unparseable — most often a CLI/daemon version skew.
    /// Operator action: rebuild and restart `cqs-watch`.
    #[error("daemon returned malformed response: {0}")]
    BadResponse(String),
    /// The daemon returned `{"status": "err", "message": "..."}` — a
    /// handler-level error. The `message` is the daemon's own description.
    #[error("daemon error: {0}")]
    DaemonError(String),
    /// The daemon's response filled the bridge read cap before a newline
    /// arrived. The payload is valid but larger than the relay buffer, so it
    /// would parse as garbage if forwarded — distinct from [`Self::BadResponse`]
    /// so the caller can advise raising `CQS_DAEMON_MAX_RESPONSE_BYTES` (or
    /// narrowing the query) rather than chasing a version-skew parse failure.
    /// The `message` names the resolved cap and the override env var.
    #[error("daemon response too large: {0}")]
    ResponseTooLarge(String),
}

#[cfg(unix)]
impl DaemonRpcError {
    /// Render the daemon error as a stable wire-format string for
    /// callers that surface it as plain text (e.g. the `cqs hook fire`
    /// fallback that touches `.cqs/.dirty`).
    pub fn as_message(&self) -> String {
        self.to_string()
    }
}

/// Daemon healthcheck response — the payload returned by `cqs ping`.
///
/// Makes the daemon's runtime state observable from the CLI without grepping
/// `journalctl` for the right span. Exposed in the library crate (rather than
/// `cli::`) so the in-tree `tests/daemon_forward_test.rs` integration tests
/// and sibling tooling (e.g. `cqs doctor --verbose`) can deserialize the same
/// shape the daemon writes.
///
/// Wire format: serialized as the `output` field of the existing
/// `{"status":"ok","output":<json>}` daemon envelope. The daemon special-cases
/// `command == "ping"` to emit a JSON object matching this struct rather than
/// going through the batch dispatcher's free-form value path.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct PingResponse {
    /// Embedding model the daemon currently has loaded (e.g.
    /// `"BAAI/bge-large-en-v1.5"`). Comes from `BatchContext::model_config.name`.
    pub model: String,
    /// Embedding dimensionality the daemon reports for that model. Sourced
    /// from the resolved `ModelConfig::dim` so it reflects any
    /// `CQS_EMBEDDING_DIM` override at daemon startup.
    pub dim: u32,
    /// Seconds since the daemon's `BatchContext` was created. Approximates
    /// "how long has the daemon been serving" — process uptime if the daemon
    /// has been up since boot, otherwise time since last restart.
    pub uptime_secs: u64,
    /// Unix timestamp (UTC seconds) of the last write to `index.db`, or
    /// `None` if the file is missing/unreadable. Best-effort proxy for
    /// "when did the index last change" — reflects both `cqs index` and
    /// incremental `cqs watch` updates because both touch the DB file.
    ///
    /// Accepts `"last_synced_at"` as a deserialization alias so a consumer
    /// reading the [`crate::watch_status::WatchSnapshot`] field name can hand
    /// the same JSON to `serde::from_value::<PingResponse>`. Canonical name in
    /// serialization stays `last_indexed_at` — the alias is read-only.
    #[serde(alias = "last_synced_at")]
    pub last_indexed_at: Option<i64>,
    /// Cumulative count of dispatch errors observed by the daemon since
    /// the `BatchContext` was created. Includes parse failures and handler
    /// errors; transport-level failures (closed sockets) are not counted.
    pub error_count: u64,
    /// Cumulative count of socket queries the daemon has dispatched since
    /// the `BatchContext` was created. Counts every line that reached
    /// `dispatch_line` whether it succeeded or errored.
    pub total_queries: u64,
    /// Whether the SPLADE encoder ONNX session is currently resident.
    /// Lazy-loaded on first sparse-needing query; once loaded it stays
    /// until idle eviction. `false` means the daemon hasn't run a query
    /// that needed sparse retrieval yet, or the SPLADE model isn't
    /// configured at all.
    pub splade_loaded: bool,
    /// Whether the cross-encoder reranker ONNX session is currently
    /// resident. Same lazy-load semantics as `splade_loaded`.
    pub reranker_loaded: bool,
}

/// Connect to the running daemon and request a `PingResponse`.
///
/// Returns `Ok(PingResponse)` on success or a typed [`DaemonRpcError`] on
/// failure (socket missing, transport/connection failure, malformed response,
/// or daemon-side error).
///
/// Reusable from `cqs doctor --verbose` and any tool that wants to ask the
/// daemon "are you alive and serving the right model?". Stays in the library
/// crate (not `cli::`) so non-binary callers can use it.
///
/// Note: this function is unix-only because the daemon socket is unix-only.
/// On non-unix platforms the daemon never starts in the first place.
#[cfg(unix)]
pub fn daemon_ping(cqs_dir: &std::path::Path) -> Result<PingResponse, DaemonRpcError> {
    daemon_request(cqs_dir, "ping", serde_json::json!([]), "PingResponse")
}

/// Generic socket round-trip helper for daemon RPCs that follow the
/// `{"status":"ok","output":<dispatch envelope>}` wire shape. Centralises
/// the connect → set_timeouts → write → read → parse → unwrap_dispatch
/// pipeline so [`daemon_ping`], [`daemon_status`], [`daemon_reconcile`],
/// and any future RPC sit on the same well-tested transport.
///
/// Inputs:
/// - `command`: the wire-level command name (`"ping"`, `"status"`, etc.)
/// - `args`: JSON value used as the request's `args` field. `json!([])`
///   for arg-less RPCs; `json!(["--hook", "post-checkout", ...])` for
///   the `reconcile` arg-vector shape.
/// - `payload_label`: type name used in error messages and the
///   `unwrap_dispatch_payload` warn arm. Conventionally the deserialised
///   type's bare name (`"PingResponse"`, `"WatchSnapshot"`).
///
/// All connect / set_timeout / read / write failures emit a
/// `tracing::debug!(stage=…, command=…)` line so a multi-project agent
/// loop polling several daemons can correlate by `command` field. Parse
/// / non-ok-status failures emit `tracing::warn!` because they signal a
/// real wire-format break, not a transient socket condition. The 5s
/// timeout and 64 KiB read cap bound every RPC.
///
/// `T` must implement `serde::Deserialize` for the inner payload (the
/// `"data"` field of the dispatch envelope, or the bare value in the
/// legacy mock-test path — see [`unwrap_dispatch_payload`]).
#[cfg(unix)]
fn daemon_request<T: serde::de::DeserializeOwned>(
    cqs_dir: &std::path::Path,
    command: &str,
    args: serde_json::Value,
    payload_label: &str,
) -> Result<T, DaemonRpcError> {
    daemon_request_with_timeout(cqs_dir, command, args, payload_label, None)
}

/// Raw socket round-trip for the MCP bridge: send a JSON-args frame
/// (`{"command": <cmd>, "arguments": {...}}`, the Lane 1 / D3-b shape) and
/// return the FULL daemon response envelope as a `serde_json::Value` —
/// `{"status":"ok","output":<dispatch envelope>}` on success or a typed
/// error on a transport/parse failure.
///
/// Unlike [`daemon_request`], this does NOT peel the dispatch layer via
/// [`unwrap_dispatch_payload`]: the bridge must inspect the un-peeled `output`
/// to distinguish a handler error riding under `status:"ok"` (the slim error
/// envelope `{"error":{...}}` — error-present, no `data` key) from a success
/// (`{"data":...}`), and to carry the envelope `_meta` through. Peeling would
/// collapse both into a bare payload and discard `_meta`.
///
/// Sizes its read cap from [`crate::limits::max_daemon_response_bytes`]
/// — the same env-overridable resolver the CLI daemon-forward path uses — so
/// the bridge is never narrower than the CLI it fronts: a valid `gather`/
/// `search`/`scout` payload the daemon would deliver to the CLI is delivered
/// to MCP too. Hitting the cap yields a distinct
/// [`DaemonRpcError::ResponseTooLarge`] naming the limit and the
/// `CQS_DAEMON_MAX_RESPONSE_BYTES` override, not an opaque parse failure.
/// Uses the shared [`resolve_daemon_timeout_ms`] timeout because tool
/// responses (search, gather, task) are far larger than the small ping/status
/// RPCs the typed helper serves.
///
/// Returns a typed [`DaemonRpcError`] on a transport/parse failure — the
/// bridge maps `SocketMissing`/`Transport`/`DaemonError`/`BadResponse` to the
/// appropriate JSON-RPC protocol error.
#[cfg(unix)]
pub fn daemon_json_args_request(
    cqs_dir: &std::path::Path,
    command: &str,
    arguments: &serde_json::Value,
) -> Result<serde_json::Value, DaemonRpcError> {
    use std::io::{BufRead, Read as _, Write};
    use std::os::unix::net::UnixStream;

    let sock_path = daemon_socket_path(cqs_dir);
    let _span = tracing::info_span!(
        "daemon_json_args_request",
        command,
        path = %sock_path.display()
    )
    .entered();

    if !sock_path.exists() {
        tracing::warn!(
            stage = "socket_missing",
            command,
            "daemon json-args request failed: socket does not exist"
        );
        return Err(DaemonRpcError::SocketMissing(format!(
            "no daemon running (socket {} does not exist)",
            sock_path.display()
        )));
    }

    let mut stream = UnixStream::connect(&sock_path).map_err(|e| {
        tracing::warn!(stage = "connect", error = %e, command, "daemon json-args request failed");
        DaemonRpcError::Transport(format!("connect to {} failed: {e}", sock_path.display()))
    })?;

    let timeout = resolve_daemon_timeout_ms();
    if let Err(e) = stream.set_read_timeout(Some(timeout)) {
        tracing::warn!(stage = "set_read_timeout", error = %e, command, "daemon json-args request failed");
        return Err(DaemonRpcError::Transport(format!(
            "set_read_timeout failed: {e}"
        )));
    }
    if let Err(e) = stream.set_write_timeout(Some(timeout)) {
        tracing::warn!(stage = "set_write_timeout", error = %e, command, "daemon json-args request failed");
        return Err(DaemonRpcError::Transport(format!(
            "set_write_timeout failed: {e}"
        )));
    }

    // The Lane 1 (D3-b) JSON-args frame: an `arguments` OBJECT deserializes
    // directly into the command's Phase-0 core struct on the daemon side.
    let request = serde_json::json!({"command": command, "arguments": arguments});
    writeln!(stream, "{}", request).map_err(|e| {
        tracing::warn!(stage = "write", error = %e, command, "daemon json-args request failed");
        DaemonRpcError::Transport(format!("write request failed: {e}"))
    })?;
    stream.flush().map_err(|e| {
        tracing::warn!(stage = "flush", error = %e, command, "daemon json-args request failed");
        DaemonRpcError::Transport(format!("flush failed: {e}"))
    })?;

    // Response read cap, sized from the shared env-overridable resolver the CLI
    // daemon-forward path uses (default 16 MiB). The request-line cap and this
    // response cap are independent limits: the former bounds the inbound
    // arguments frame, this bounds the outbound payload the bridge buffers.
    // Sizing from the shared resolver keeps the bridge no narrower than the CLI
    // it fronts — a valid gather/search/scout payload that reaches the CLI
    // reaches MCP too.
    let max_response = crate::limits::max_daemon_response_bytes();
    // Read one byte past the cap so the cap-hit test is `> max_response`, not
    // `== max_response`: a valid payload that is exactly `max_response` bytes
    // (its terminating newline included) must be accepted, not mistaken for a
    // truncated one. Only reading past the cap means the payload genuinely
    // exceeded the limit.
    let mut reader = std::io::BufReader::new(&stream).take(max_response.saturating_add(1));
    let mut response_line = String::new();
    let bytes_read = reader.read_line(&mut response_line).map_err(|e| {
        tracing::warn!(stage = "read", error = %e, command, "daemon json-args request failed");
        DaemonRpcError::Transport(format!("read response failed: {e}"))
    })?;

    // Reading more than the cap means the payload was truncated mid-line.
    // Forwarding it would parse as garbage; surface a distinct error naming the
    // limit and the override knob so a large-but-valid result is not mistaken
    // for a malformed daemon response.
    if bytes_read as u64 > max_response {
        let cap_mib = max_response / 1024 / 1024;
        tracing::warn!(
            stage = "read",
            bytes = bytes_read,
            cap_mib,
            command,
            "daemon json-args response exceeded read cap"
        );
        return Err(DaemonRpcError::ResponseTooLarge(format!(
            "response exceeded {cap_mib} MiB — raise CQS_DAEMON_MAX_RESPONSE_BYTES or narrow the query"
        )));
    }

    let envelope: serde_json::Value = serde_json::from_str(response_line.trim()).map_err(|e| {
        tracing::warn!(stage = "parse", error = %e, command, "daemon json-args request failed");
        DaemonRpcError::BadResponse(format!("parse envelope failed: {e}"))
    })?;

    let status = envelope
        .get("status")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            tracing::warn!(
                stage = "parse",
                command,
                "daemon json-args request failed: missing status field"
            );
            DaemonRpcError::BadResponse("missing 'status' field in daemon response".to_string())
        })?;

    if status != "ok" {
        // A non-ok status is a transport/parse failure on the socket layer
        // (NUL bytes, bad relay, missing command). The daemon's `message` is
        // already privacy-redacted. Handler-semantic errors do NOT come this
        // way — they ride under status:"ok" inside `output` (Blocker #1).
        let msg = envelope
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("daemon error")
            .to_string();
        tracing::warn!(
            stage = "parse",
            status,
            command,
            "daemon json-args request failed: non-ok status"
        );
        return Err(DaemonRpcError::DaemonError(msg));
    }

    // Return the full envelope un-peeled — the bridge classifies `output`.
    Ok(envelope)
}

/// Like [`daemon_request`] but accepts a per-call read/write timeout
/// override. `None` uses the default 5 s; `Some(d)` sets both timeouts
/// to `d`. Used by [`wait_for_fresh`] where the daemon holds the connection
/// while parking on its `FreshNotifier` for up to the caller's wait budget.
///
/// The override applies before the request is written, so a misbehaving
/// daemon that never replies still hits a deadline. Callers should pass
/// `wait_secs + small_grace` so the client-side timeout can't beat the
/// server-side `wait_until_fresh` deadline by accident.
#[cfg(unix)]
fn daemon_request_with_timeout<T: serde::de::DeserializeOwned>(
    cqs_dir: &std::path::Path,
    command: &str,
    args: serde_json::Value,
    payload_label: &str,
    timeout_override: Option<std::time::Duration>,
) -> Result<T, DaemonRpcError> {
    use std::io::{BufRead, Read as _, Write};
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    let sock_path = daemon_socket_path(cqs_dir);
    // Single span for every daemon RPC, with the wire `command` as a
    // structured field so a multi-project loop can grep by command without
    // per-function span proliferation.
    let _span = tracing::info_span!(
        "daemon_request",
        command = command,
        path = %sock_path.display()
    )
    .entered();
    if !sock_path.exists() {
        return Err(DaemonRpcError::SocketMissing(format!(
            "no daemon running (socket {} does not exist)",
            sock_path.display()
        )));
    }

    let mut stream = UnixStream::connect(&sock_path).map_err(|e| {
        // Connect failures are debug, not warn: the `wait_for_fresh` polling
        // loop hits this path repeatedly during daemon startup, and warn-level
        // would flood journalctl. Final-decision warns live in
        // `wait_for_fresh` / the eval gate.
        tracing::debug!(stage = "connect", error = %e, command, "daemon request failed");
        DaemonRpcError::Transport(format!("connect to {} failed: {e}", sock_path.display()))
    })?;

    // 5s is generous: every daemon RPC's handler does at most a single
    // RwLock read + clone + a `metadata()` syscall. No real I/O on the
    // daemon side.
    //
    // `wait_fresh` parks server-side for up to `wait_secs` before responding,
    // so the caller passes a longer timeout via `timeout_override`. All other
    // RPCs leave it `None` and inherit the 5 s default.
    let timeout = timeout_override.unwrap_or(Duration::from_secs(5));
    if let Err(e) = stream.set_read_timeout(Some(timeout)) {
        tracing::debug!(stage = "set_read_timeout", error = %e, command, "daemon request failed");
        return Err(DaemonRpcError::Transport(format!(
            "set_read_timeout failed: {e}"
        )));
    }
    if let Err(e) = stream.set_write_timeout(Some(timeout)) {
        tracing::debug!(stage = "set_write_timeout", error = %e, command, "daemon request failed");
        return Err(DaemonRpcError::Transport(format!(
            "set_write_timeout failed: {e}"
        )));
    }

    let request = serde_json::json!({"command": command, "args": args});
    writeln!(stream, "{}", request).map_err(|e| {
        tracing::debug!(stage = "write", error = %e, command, "daemon request failed");
        DaemonRpcError::Transport(format!("write request failed: {e}"))
    })?;
    stream.flush().map_err(|e| {
        tracing::debug!(stage = "flush", error = %e, command, "daemon request failed");
        DaemonRpcError::Transport(format!("flush failed: {e}"))
    })?;

    // 64 KiB read cap. Responses are small (<1 KB for ping, ~few KB for
    // status); the cap is a defensive ceiling against a buggy daemon, not a
    // sizing knob.
    let mut reader = std::io::BufReader::new(&stream).take(64 * 1024);
    let mut response_line = String::new();
    reader.read_line(&mut response_line).map_err(|e| {
        tracing::debug!(stage = "read", error = %e, command, "daemon request failed");
        DaemonRpcError::Transport(format!("read response failed: {e}"))
    })?;

    let envelope: serde_json::Value = serde_json::from_str(response_line.trim()).map_err(|e| {
        tracing::warn!(stage = "parse", error = %e, command, "daemon request failed");
        DaemonRpcError::BadResponse(format!("parse envelope failed: {e}"))
    })?;

    let status = envelope
        .get("status")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            tracing::warn!(
                stage = "parse",
                command,
                "daemon request failed: missing status field"
            );
            DaemonRpcError::BadResponse("missing 'status' field in daemon response".to_string())
        })?;
    if status != "ok" {
        return Err(match envelope.get("message") {
            // String message: the daemon's own handler-level description.
            Some(serde_json::Value::String(msg)) => {
                tracing::warn!(
                    stage = "parse",
                    status,
                    msg = msg.as_str(),
                    command,
                    "daemon request failed: non-ok status"
                );
                DaemonRpcError::DaemonError(msg.clone())
            }
            // Present but not a string: a malformed envelope, surfaced as a
            // shape mismatch rather than silently collapsed to a placeholder.
            Some(other) => {
                tracing::warn!(
                    stage = "parse",
                    status,
                    command,
                    "daemon request failed: non-ok status with non-string message"
                );
                DaemonRpcError::BadResponse(format!(
                    "non-ok daemon response carried a non-string 'message' field: {other}"
                ))
            }
            // No message field at all: report the absence rather than echoing
            // a placeholder that renders as the doubled "daemon error: ...".
            None => {
                tracing::warn!(
                    stage = "parse",
                    status,
                    command,
                    "daemon request failed: non-ok status with no message field"
                );
                DaemonRpcError::BadResponse(format!(
                    "non-ok daemon response (status {status:?}) omitted the 'message' field"
                ))
            }
        });
    }

    let output = envelope.get("output").ok_or_else(|| {
        tracing::warn!(
            stage = "parse",
            command,
            "daemon request failed: missing output field"
        );
        DaemonRpcError::BadResponse("missing 'output' field in daemon response".to_string())
    })?;
    let payload =
        unwrap_dispatch_payload(output, payload_label).map_err(DaemonRpcError::BadResponse)?;
    serde_json::from_value::<T>(payload).map_err(|e| {
        DaemonRpcError::BadResponse(format!("{payload_label} deserialize failed: {e}"))
    })
}

/// Pull the inner handler payload out of a daemon dispatch response.
///
/// The wire chain looks like:
///
/// ```text
/// outer:    {"status":"ok","output":<inner>}        // socket layer
/// inner:    {"data":<payload>,"error":null,"version":N,"_meta":{...}}
///           // batch dispatch envelope (write_json_line)
/// ```
///
/// `<inner>` may arrive in two forms:
///
/// 1. **Object form** (production daemon socket): `output` parses to a
///    JSON object — the dispatch envelope. The handler payload is at
///    `output.data`.
/// 2. **String form** (some integration test mocks, and any caller that
///    re-wraps the dispatch bytes verbatim): `output` is a JSON string
///    that needs a second `from_str` to reach the inner shape.
///
/// We accept both forms transparently. A dispatch envelope that carries a
/// non-null `error` field surfaces that error rather than returning its
/// (typically null) `data` payload. If neither an `error` nor a `data` field
/// is present — i.e. the value is already the bare handler payload — return
/// it as-is so the legacy mock-test path keeps working.
//
// Compiled on unix (its only production caller, `daemon_request_with_timeout`,
// is `#[cfg(unix)]`) and under any test build (the unit tests below are
// cross-platform `#[test]`). On a non-unix release build it has no caller, so
// gating it here keeps `clippy -D warnings` (run per-target in release.yml)
// from tripping dead_code on the Windows cross-build.
#[cfg(any(unix, test))]
fn unwrap_dispatch_payload(
    output: &serde_json::Value,
    type_name: &str,
) -> Result<serde_json::Value, String> {
    let parsed: serde_json::Value = match output {
        serde_json::Value::String(s) => serde_json::from_str(s).map_err(|e| {
            tracing::warn!(stage = "parse", error = %e, type_name, "daemon dispatch output JSON parse failed");
            format!("parse {type_name} output JSON failed: {e}")
        })?,
        other => other.clone(),
    };
    if let Some(obj) = parsed.as_object() {
        // A non-null `error` in the dispatch envelope is a handler failure;
        // surface it instead of returning the accompanying (null) `data`.
        if let Some(err) = obj.get("error").filter(|e| !e.is_null()) {
            tracing::warn!(
                stage = "parse",
                type_name,
                "daemon dispatch envelope carried an error"
            );
            return Err(format!("daemon dispatch error for {type_name}: {err}"));
        }
        // The dispatch envelope: dig out `data`.
        if let Some(data) = obj.get("data") {
            return Ok(data.clone());
        }
    }
    // Pass through (legacy bare-payload mock form).
    Ok(parsed)
}

/// Connect to the running daemon and request a [`WatchSnapshot`].
///
/// Mirrors [`daemon_ping`] in shape (same envelope, same string-payload
/// transport) but issues the `status` command and deserializes a
/// [`WatchSnapshot`]. The daemon path is chosen because that's where the
/// watch loop publishes from — the CLI-only path returns a default
/// `unknown` snapshot, which would defeat the purpose.
///
/// Returns a typed [`DaemonRpcError`] on failure. Callers can fall back
/// or surface verbatim. Connect-stage failures are `tracing::debug!` (not
/// warn) because [`wait_for_fresh`] polls this in a tight loop during startup;
/// final-decision warns live in [`wait_for_fresh`] / the eval gate.
///
/// Unix-only: the daemon socket is unix-only.
///
/// [`WatchSnapshot`]: crate::watch_status::WatchSnapshot
#[cfg(unix)]
pub fn daemon_status(
    cqs_dir: &std::path::Path,
) -> Result<crate::watch_status::WatchSnapshot, DaemonRpcError> {
    daemon_request(cqs_dir, "status", serde_json::json!([]), "WatchSnapshot")
}

/// Response shape for [`daemon_reconcile`]. Mirrors the JSON envelope
/// `dispatch_reconcile` returns: a confirmation that the signal was accepted
/// plus the advisory hook metadata. "Was the signal accepted?" is surfaced via
/// `Result::Ok` rather than a dedicated field.
#[cfg(unix)]
#[derive(Debug, Clone, serde::Deserialize)]
pub struct DaemonReconcileResponse {
    /// `true` if a previous request was still pending when this call
    /// arrived. Surfaces coalescing for hook-burst scenarios (rebase).
    pub was_pending: bool,
    /// Echoed advisory fields. Useful for tracing in the hook script's
    /// stderr.
    pub hook: Option<String>,
    pub args: Vec<String>,
}

/// Post a `reconcile` socket message to the running daemon. Used by the
/// `cqs hook fire` CLI surface.
///
/// Returns the parsed [`DaemonReconcileResponse`] on success, or a typed
/// [`DaemonRpcError`] on failure. The CLI surface downgrades the
/// `SocketMissing` variant to the `.cqs/.dirty` fallback; every other
/// variant is surfaced verbatim so hooks fail loudly.
#[cfg(unix)]
pub fn daemon_reconcile(
    cqs_dir: &std::path::Path,
    hook: Option<&str>,
    args: &[String],
) -> Result<DaemonReconcileResponse, DaemonRpcError> {
    // Build the batch-arg vector: ["--hook", "<name>", "--arg", "v1",
    // "--arg", "v2", ...]. The batch parser accepts `--arg` repeated
    // for `Vec<String>` fields.
    let mut batch_args: Vec<String> = Vec::with_capacity(2 + 2 * args.len());
    if let Some(name) = hook {
        batch_args.push("--hook".to_string());
        batch_args.push(name.to_string());
    }
    for a in args {
        batch_args.push("--arg".to_string());
        batch_args.push(a.clone());
    }
    daemon_request(
        cqs_dir,
        "reconcile",
        serde_json::json!(batch_args),
        "DaemonReconcileResponse",
    )
}

/// Outcome of [`wait_for_fresh`].
///
/// Five cases callers need to distinguish so the caller-side advice
/// matches the actual failure mode:
/// - `Fresh` — daemon reported `state == fresh` within the budget; safe to
///   proceed with the gated work.
/// - `Timeout` — daemon was reachable but never became fresh in time. The
///   final snapshot is attached so callers can surface counters
///   (`modified_files`, `pending_notes`) in their error message.
/// - `NoDaemon` — socket file does not exist. Operator action: start
///   `cqs watch --serve`.
/// - `Transport` — socket exists but the daemon isn't responding (connect /
///   read / write / timeout). Operator action: check the daemon log,
///   consider restarting the unit.
/// - `BadResponse` — daemon answered but the response was unparseable.
///   Most often a CLI/daemon version skew — restart `cqs-watch`.
#[cfg(unix)]
#[derive(Debug, Clone)]
pub enum FreshnessWait {
    Fresh(crate::watch_status::WatchSnapshot),
    Timeout(crate::watch_status::WatchSnapshot),
    /// Socket file missing — the daemon never started.
    NoDaemon(String),
    /// Connect/read/write/timeout — daemon is gone or hung.
    Transport(String),
    /// Envelope/JSON/parse error — daemon answered but garbled.
    BadResponse(String),
}

/// Shared client-side wait for `state == fresh`.
///
/// Both `cqs status --watch-fresh --wait` and `cqs eval --require-fresh`
/// route through this. The budget is capped at 86,400 s (24 h) defensively so
/// a misconfigured caller can't pass a value that overflows
/// `Instant + Duration::from_secs`.
///
/// The first successful poll that reports `fresh` returns immediately —
/// callers don't pay any latency on an already-fresh tree.
///
/// Errors are surfaced per [`DaemonRpcError`] variant rather than collapsed
/// into a single `NoDaemon` so the caller-side advice can distinguish "no
/// daemon at all" (start one), "daemon hung" (restart), and "daemon answered
/// with garbage" (version skew → restart). Final-decision warns are emitted
/// here, one per terminal outcome.
#[cfg(unix)]
pub fn wait_for_fresh(cqs_dir: &std::path::Path, wait_secs: u64) -> FreshnessWait {
    let _span = tracing::info_span!("wait_for_fresh", wait_secs).entered();
    let start = std::time::Instant::now();
    // Defensive cap so a `pub fn` can't panic on
    // `Instant + Duration::from_secs(u64::MAX)`. The daemon-side
    // `dispatch_wait_fresh` mirrors the same cap so a malicious / buggy
    // client can't request a wait longer than 24 h either.
    let bounded_secs = wait_secs.min(86_400);

    // Deadline-first: a zero-budget wait short-circuits to `Timeout(unknown)`
    // without a socket round-trip. Real callers always pass a non-zero budget;
    // the zero path is a defensive contract that "give me 0 budget" never
    // spawns network I/O.
    if bounded_secs == 0 {
        tracing::info!(
            elapsed_ms = start.elapsed().as_millis() as u64,
            "wait_for_fresh: deadline reached before first fresh poll",
        );
        return FreshnessWait::Timeout(crate::watch_status::WatchSnapshot::unknown());
    }

    // Single-round-trip server-side wait. The daemon parks the request on its
    // shared `FreshNotifier` until either the watch loop publishes a Fresh
    // transition or the deadline expires, then replies with the current
    // snapshot.
    //
    // Client-side timeout is `bounded_secs + 5` so a slow-but-honest
    // daemon (handler scheduling jitter, write-buffer flush) gets a
    // small grace period before the read times out. A genuinely wedged
    // daemon still hits the budget plus grace, not minutes-of-pin.
    let timeout = std::time::Duration::from_secs(bounded_secs.saturating_add(5));
    let request = serde_json::json!(["--wait-secs", bounded_secs.to_string()]);
    let result: Result<crate::watch_status::WatchSnapshot, DaemonRpcError> =
        daemon_request_with_timeout(
            cqs_dir,
            "wait-fresh",
            request,
            "WatchSnapshot",
            Some(timeout),
        );

    match result {
        Ok(snap) => {
            if snap.is_fresh() {
                tracing::info!(
                    elapsed_ms = start.elapsed().as_millis() as u64,
                    modified_files = snap.modified_files,
                    "wait_for_fresh: index reached Fresh",
                );
                FreshnessWait::Fresh(snap)
            } else {
                tracing::info!(
                    elapsed_ms = start.elapsed().as_millis() as u64,
                    modified_files = snap.modified_files,
                    pending_notes = snap.pending_notes,
                    rebuild_in_flight = snap.rebuild_in_flight,
                    "wait_for_fresh: timeout — index still stale",
                );
                FreshnessWait::Timeout(snap)
            }
        }
        Err(DaemonRpcError::SocketMissing(msg)) => {
            tracing::info!(error = %msg, "wait_for_fresh: daemon socket missing");
            FreshnessWait::NoDaemon(msg)
        }
        Err(DaemonRpcError::Transport(msg)) => {
            tracing::info!(error = %msg, "wait_for_fresh: transport failure");
            FreshnessWait::Transport(msg)
        }
        Err(DaemonRpcError::BadResponse(msg)) => {
            tracing::warn!(error = %msg, "wait_for_fresh: malformed daemon response");
            FreshnessWait::BadResponse(msg)
        }
        Err(DaemonRpcError::DaemonError(msg)) => {
            tracing::warn!(error = %msg, "wait_for_fresh: daemon-side error");
            FreshnessWait::Transport(msg)
        }
        Err(DaemonRpcError::ResponseTooLarge(msg)) => {
            // The small status/reconcile payloads this path reads cannot
            // realistically fill the cap; treat an over-cap response as an
            // unusable daemon answer.
            tracing::warn!(error = %msg, "wait_for_fresh: daemon response exceeded read cap");
            FreshnessWait::BadResponse(msg)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(tokens: &[&str]) -> Vec<String> {
        tokens.iter().map(|s| s.to_string()).collect()
    }

    /// Hand-built spec mirroring the production classification (derived from
    /// the live clap definition in `cli::dispatch`). Subset is enough for the
    /// translation-rule tests here; the full derived spec is exercised
    /// end-to-end by `tests/daemon_forward_test.rs`.
    fn spec() -> CliArgSpec {
        let set = |items: &[&str]| {
            items
                .iter()
                .map(|s| s.to_string())
                .collect::<std::collections::BTreeSet<String>>()
        };
        CliArgSpec {
            value_flags: set(&[
                "--model",
                "--slot",
                "-n",
                "--limit",
                "-t",
                "--threshold",
                "--tokens",
                "-l",
                "--lang",
                "-p",
                "--path",
            ]),
            bare_query_strip: set(&[
                "--json",
                "-q",
                "--quiet",
                "-v",
                "--verbose",
                "--model",
                "--slot",
            ]),
            // The production spec derives these from the live clap definitions
            // (`CliArgSpec::from_clap`); the hand-built subset here mirrors the
            // scope flags and the one current forward target (`similar`) so the
            // translator-rule tests below exercise the same path. The
            // derivation itself is pinned by `scope_targets_track_the_batch_wire_surface`
            // in `cli::dispatch`.
            scope_flags: set(&["-l", "--lang", "-p", "--path"]),
            scope_targets: set(&["similar"]),
        }
    }

    // Env-var-driven timeout helper used by both daemon and CLI sides.
    // Test only the unset path; mutating env vars in a parallel test runner
    // is fragile, and the floor / honor logic is small enough to inspect.
    #[test]
    fn resolve_daemon_timeout_default_is_30s() {
        if std::env::var("CQS_DAEMON_TIMEOUT_MS").is_ok() {
            return;
        }
        assert_eq!(
            resolve_daemon_timeout_ms(),
            std::time::Duration::from_secs(30)
        );
    }

    // Sanity parity with `try_daemon_query`. The full black-box suite lives
    // in `tests/daemon_forward_test.rs`.

    #[test]
    fn strips_json_bare_query() {
        let (cmd, args) = translate_cli_args_to_batch(&v(&["--json", "search me"]), false, &spec());
        assert_eq!(cmd, "search");
        assert_eq!(args, v(&["search me"]));
    }

    /// Subcommand args forward verbatim — the batch parser flattens the same
    /// shared args structs, and `LimitArg` accepts `-n` directly. No remap:
    /// remapping `-n` → `--limit` is what broke blame, whose `-n` means
    /// `--commits`.
    #[test]
    fn forwards_subcommand_dash_n_verbatim() {
        let (cmd, args) =
            translate_cli_args_to_batch(&v(&["impact", "foo", "-n", "5"]), true, &spec());
        assert_eq!(cmd, "impact");
        assert_eq!(args, v(&["foo", "-n", "5"]));

        let (cmd, args) =
            translate_cli_args_to_batch(&v(&["blame", "foo", "-n", "3"]), true, &spec());
        assert_eq!(cmd, "blame");
        assert_eq!(args, v(&["foo", "-n", "3"]));
    }

    /// Top-level flags before the subcommand are dropped wholesale — they
    /// configure the CLI process, not the subcommand handler. Boolean flags
    /// drop one token; value flags drop two.
    #[test]
    fn drops_top_level_region_before_subcommand() {
        let (cmd, args) = translate_cli_args_to_batch(&v(&["-v", "callers", "foo"]), true, &spec());
        assert_eq!(cmd, "callers");
        assert_eq!(args, v(&["foo"]));

        let (cmd, args) =
            translate_cli_args_to_batch(&v(&["--rrf", "callers", "foo"]), true, &spec());
        assert_eq!(cmd, "callers");
        assert_eq!(args, v(&["foo"]));

        // Value flag pre-subcommand consumes its value token too — the value
        // must not be mistaken for the subcommand name.
        let (cmd, args) =
            translate_cli_args_to_batch(&v(&["-n", "3", "callers", "foo"]), true, &spec());
        assert_eq!(cmd, "callers");
        assert_eq!(args, v(&["foo"]));

        // Attached-value form is one token.
        let (cmd, args) = translate_cli_args_to_batch(
            &v(&["--model=bge-large", "callers", "foo"]),
            true,
            &spec(),
        );
        assert_eq!(cmd, "callers");
        assert_eq!(args, v(&["foo"]));
    }

    /// `similar` is the one carve-out to the drop-the-top-level-region rule:
    /// the `--lang` / `--path` scope flags reach the CLI's `cmd_similar` via the
    /// top-level region, so they're re-forwarded onto the `similar` tail (its
    /// batch `SimilarArgs` accepts them) — otherwise daemon-routed `similar`
    /// would silently ignore scoping that CLI-direct honors.
    #[test]
    fn forwards_top_level_scope_flags_for_similar() {
        // The captured scope flags are spliced in *front* of the tail (so a
        // tail-explicit value stays last-wins authoritative). clap parses
        // `--lang rust foo` identically to `foo --lang rust`.
        let (cmd, args) =
            translate_cli_args_to_batch(&v(&["--lang", "rust", "similar", "foo"]), true, &spec());
        assert_eq!(cmd, "similar");
        assert_eq!(args, v(&["--lang", "rust", "foo"]));

        // Both flags, short + long, spaced — capture order preserved, in front.
        let (cmd, args) = translate_cli_args_to_batch(
            &v(&["-l", "rust", "-p", "src/*", "similar", "foo"]),
            true,
            &spec(),
        );
        assert_eq!(cmd, "similar");
        assert_eq!(args, v(&["-l", "rust", "-p", "src/*", "foo"]));

        // Attached-value form is one token, forwarded whole (still in front).
        let (cmd, args) = translate_cli_args_to_batch(
            &v(&["--path=src/store/", "similar", "foo"]),
            true,
            &spec(),
        );
        assert_eq!(cmd, "similar");
        assert_eq!(args, v(&["--path=src/store/", "foo"]));
    }

    /// The scope carve-out is `similar`-only. A non-`similar` subcommand still
    /// drops the whole top-level region (no spurious `--lang` appended), and a
    /// tail-explicit scope flag on `similar` is preserved with the top-level
    /// capture spliced in *front* so clap's last-wins keeps the tail value.
    #[test]
    fn scope_forward_is_similar_only_and_tail_wins() {
        // Non-similar subcommand: top-level `--lang` dropped, nothing appended.
        let (cmd, args) =
            translate_cli_args_to_batch(&v(&["--lang", "rust", "callers", "foo"]), true, &spec());
        assert_eq!(cmd, "callers");
        assert_eq!(args, v(&["foo"]));

        // Tail-explicit scope flag stays authoritative: the top-level capture
        // (`--lang rust`) is spliced in *front* of the original tail
        // (`foo --lang python`), so clap resolves `--lang` to the trailing
        // tail value (`python`) by last-wins.
        let (cmd, args) = translate_cli_args_to_batch(
            &v(&["--lang", "rust", "similar", "foo", "--lang", "python"]),
            true,
            &spec(),
        );
        assert_eq!(cmd, "similar");
        assert_eq!(args, v(&["--lang", "rust", "foo", "--lang", "python"]));
    }

    /// Bare-query search knobs forward verbatim (`-n` included — the batch
    /// `search` parser accepts it via the shared `LimitArg`); process-local
    /// flags are stripped, with their value when they take one.
    #[test]
    fn bare_query_forwards_search_knobs_strips_process_local() {
        let (cmd, args) = translate_cli_args_to_batch(
            &v(&["hello", "--rrf", "-n", "8", "--model", "bge-large", "-v"]),
            false,
            &spec(),
        );
        assert_eq!(cmd, "search");
        assert_eq!(args, v(&["hello", "--rrf", "-n", "8"]));
    }

    /// `cqs -qv "hello"`: clap accepts the `-qv` cluster daemon-down, so it
    /// reaches the bare-query forward path. Both shorts are process-local
    /// (`-q`/`-v`), so the whole cluster is stripped and only the query
    /// forwards — instead of forwarding `-qv` verbatim and tripping a
    /// non-recoverable protocol error in the batch parser.
    #[test]
    fn bare_query_expands_combined_short_bools() {
        let (cmd, args) = translate_cli_args_to_batch(&v(&["-qv", "hello"]), false, &spec());
        assert_eq!(cmd, "search");
        assert_eq!(args, v(&["hello"]));
    }

    /// A combined short mixing a forwarded search knob with a process-local
    /// bool: `-vn 8` expands to `-v` (stripped) + `-n` (forwarded value flag),
    /// whose value `8` is the next spaced token. The forwarded `-n 8` survives.
    #[test]
    fn bare_query_expands_combined_short_mixed_with_value_flag() {
        let (cmd, args) = translate_cli_args_to_batch(&v(&["hello", "-vn", "8"]), false, &spec());
        assert_eq!(cmd, "search");
        assert_eq!(args, v(&["hello", "-n", "8"]));
    }

    /// Attached-value cluster: `-qn8` expands to `-q` (stripped) + `-n8` (the
    /// remainder is the attached value), forwarded as `-n=8`.
    #[test]
    fn bare_query_expands_combined_short_with_attached_value() {
        let (cmd, args) = translate_cli_args_to_batch(&v(&["hello", "-qn8"]), false, &spec());
        assert_eq!(cmd, "search");
        assert_eq!(args, v(&["hello", "-n=8"]));
    }

    /// All-forwarded cluster of bool knobs survives intact: `-ab` where both
    /// are search knobs (not in `bare_query_strip`) expands and re-emits both.
    #[test]
    fn bare_query_expands_combined_short_all_forwarded() {
        // `-q` is process-local; pair it with a non-stripped bool short by
        // using a spec spelling that forwards. `-v` is stripped, so use two
        // forwarded shorts via the value-flag-free path: emulate with `-tl`
        // — but `-t`/`-l` take values, so use a pure pass-through pair.
        // The cluster `-xy` (neither known) forwards each short verbatim for
        // the batch parser to own the rejection.
        let (cmd, args) = translate_cli_args_to_batch(&v(&["hello", "-xy"]), false, &spec());
        assert_eq!(cmd, "search");
        assert_eq!(args, v(&["hello", "-x", "-y"]));
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

    /// `cqs notes list` (the CLI form) must reach the daemon as `notes`,
    /// not `notes list` — the batch parser's `BatchCmd::Notes` accepts
    /// `--warnings`/`--patterns` directly without a `list` subcommand.
    /// Without the strip, every `cqs notes list` through the daemon errors
    /// `unexpected argument 'list' found`. Keep the explicit
    /// `--warnings`/`--patterns` flags through unchanged.
    #[test]
    fn notes_list_subcommand_stripped() {
        let (cmd, args) = translate_cli_args_to_batch(&v(&["notes", "list"]), true, &spec());
        assert_eq!(cmd, "notes");
        assert!(args.is_empty(), "got {args:?}");
    }

    #[test]
    fn notes_list_with_warnings_strips_only_list() {
        let (cmd, args) =
            translate_cli_args_to_batch(&v(&["notes", "list", "--warnings"]), true, &spec());
        assert_eq!(cmd, "notes");
        assert_eq!(args, v(&["--warnings"]));
    }

    /// Bare `cqs notes` (no `list` token) is unaffected.
    #[test]
    fn notes_without_list_unchanged() {
        let (cmd, args) = translate_cli_args_to_batch(&v(&["notes", "--patterns"]), true, &spec());
        assert_eq!(cmd, "notes");
        assert_eq!(args, v(&["--patterns"]));
    }

    /// Other commands with a `list` first-arg are NOT touched — only `notes`.
    #[test]
    fn list_arg_is_only_stripped_for_notes() {
        let (cmd, args) = translate_cli_args_to_batch(&v(&["impact", "list"]), true, &spec());
        assert_eq!(cmd, "impact");
        assert_eq!(args, v(&["list"]));
    }

    /// Smoke-test PingResponse round-trips through serde without schema drift.
    /// Pins the wire shape so a field rename here doesn't silently break the
    /// CLI<->daemon contract.
    #[test]
    fn ping_response_roundtrip_serde() {
        let original = PingResponse {
            model: "BAAI/bge-large-en-v1.5".to_string(),
            dim: 1024,
            uptime_secs: 9_375,
            last_indexed_at: Some(1_734_120_000),
            error_count: 3,
            total_queries: 12_453,
            splade_loaded: true,
            reranker_loaded: false,
        };
        let json = serde_json::to_string(&original).unwrap();
        // Spot-check field names: a typo here would be a wire-break.
        assert!(json.contains("\"model\""));
        assert!(json.contains("\"dim\""));
        assert!(json.contains("\"uptime_secs\""));
        assert!(json.contains("\"last_indexed_at\""));
        assert!(json.contains("\"error_count\""));
        assert!(json.contains("\"total_queries\""));
        assert!(json.contains("\"splade_loaded\""));
        assert!(json.contains("\"reranker_loaded\""));
        let parsed: PingResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, original);
    }

    /// PingResponse with no last-indexed timestamp serializes as
    /// `"last_indexed_at": null` and round-trips. Pins the Option<i64>
    /// behaviour so the CLI doesn't have to special-case missing-field
    /// vs. null vs. -1 sentinel.
    #[test]
    fn ping_response_null_last_indexed() {
        let original = PingResponse {
            model: "test".into(),
            dim: 1,
            uptime_secs: 0,
            last_indexed_at: None,
            error_count: 0,
            total_queries: 0,
            splade_loaded: false,
            reranker_loaded: false,
        };
        let json = serde_json::to_string(&original).unwrap();
        assert!(
            json.contains("\"last_indexed_at\":null"),
            "Option::None must serialize as JSON null, got {json}"
        );
        let parsed: PingResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.last_indexed_at, None);
    }

    /// `daemon_ping` must surface a friendly error (not a panic or generic IO
    /// message) when the socket file is absent. This is the primary failure
    /// mode the CLI hits when no daemon is running.
    #[cfg(unix)]
    #[test]
    fn daemon_ping_errors_without_socket() {
        let dir = tempfile::tempdir().unwrap();
        // No XDG_RUNTIME_DIR override here — the helper hashes the cqs_dir
        // path, so even if there's a real daemon socket somewhere the
        // hashed name for this temp dir won't collide.
        let cqs_dir = dir.path().join(".cqs");
        std::fs::create_dir_all(&cqs_dir).unwrap();
        let result = daemon_ping(&cqs_dir);
        assert!(result.is_err());
        let err = result.unwrap_err();
        // SocketMissing is the no-daemon path.
        assert!(
            matches!(err, DaemonRpcError::SocketMissing(_)),
            "expected SocketMissing variant, got: {err:?}"
        );
        let msg = err.as_message();
        assert!(
            msg.contains("no daemon running"),
            "expected friendly error, got: {msg}"
        );
    }

    /// `daemon_ping` must round-trip through a mock listener that speaks the
    /// same envelope as the real daemon. Asserts the field-by-field decoding
    /// so a drift in either side surfaces here.
    #[cfg(unix)]
    #[test]
    fn daemon_ping_mock_round_trip() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let cqs_dir = dir.path().join(".cqs");
        std::fs::create_dir_all(&cqs_dir).unwrap();

        // Thread-local override avoids the unsafe set_var race that hangs CI.
        set_socket_dir_override_for_test(Some(dir.path().to_path_buf()));

        let sock_path = daemon_socket_path(&cqs_dir);
        let listener = UnixListener::bind(&sock_path).unwrap();

        // Mock daemon thread: accept one connection, drain the request
        // line (just to confirm we got the expected JSON), reply with a
        // canned `{"status":"ok","output":"<PingResponse JSON string>"}`.
        let response_payload = serde_json::json!({
            "model": "BAAI/bge-large-en-v1.5",
            "dim": 1024,
            "uptime_secs": 60,
            "last_indexed_at": 1_734_120_000_i64,
            "error_count": 2,
            "total_queries": 100,
            "splade_loaded": true,
            "reranker_loaded": false,
        })
        .to_string();
        let envelope = serde_json::json!({
            "status": "ok",
            "output": response_payload,
        });
        let envelope_str = envelope.to_string();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request_line = String::new();
            BufReader::new(&stream)
                .read_line(&mut request_line)
                .unwrap();
            // Sanity: confirm the helper actually sent a ping request.
            assert!(
                request_line.contains("\"command\":\"ping\""),
                "expected ping request, got: {request_line}"
            );
            writeln!(stream, "{envelope_str}").unwrap();
            stream.flush().unwrap();
        });

        let result = daemon_ping(&cqs_dir);
        handle.join().unwrap();
        let _ = std::fs::remove_file(&sock_path);

        set_socket_dir_override_for_test(None);

        let resp = result.expect("daemon_ping should succeed against mock");
        assert_eq!(resp.model, "BAAI/bge-large-en-v1.5");
        assert_eq!(resp.dim, 1024);
        assert_eq!(resp.uptime_secs, 60);
        assert_eq!(resp.last_indexed_at, Some(1_734_120_000));
        assert_eq!(resp.error_count, 2);
        assert_eq!(resp.total_queries, 100);
        assert!(resp.splade_loaded);
        assert!(!resp.reranker_loaded);
    }

    /// The production daemon serializes `output` as the dispatch envelope
    /// `{"data":<payload>,"error":null,...}`, not as a JSON-string of the bare
    /// payload like the mock above. The helper `unwrap_dispatch_payload` digs
    /// into `data` so `daemon_ping` / `daemon_status` deserialize the actual
    /// handler value. Pin both paths here.
    #[test]
    fn unwrap_dispatch_payload_extracts_data_from_envelope_object() {
        let envelope = serde_json::json!({
            "data": {"model": "x", "n": 1},
            "error": null,
            "version": 1,
            "_meta": {"handling_advice": "..."}
        });
        let inner = unwrap_dispatch_payload(&envelope, "X").unwrap();
        assert_eq!(inner, serde_json::json!({"model": "x", "n": 1}));
    }

    #[test]
    fn unwrap_dispatch_payload_passes_through_bare_object() {
        // Legacy mock form: `output` is already the bare payload (no `data`
        // key). Helper must return it unchanged so existing test mocks keep
        // working.
        let bare = serde_json::json!({"model": "x", "n": 1});
        let inner = unwrap_dispatch_payload(&bare, "X").unwrap();
        assert_eq!(inner, bare);
    }

    #[test]
    fn unwrap_dispatch_payload_parses_string_then_extracts_data() {
        // String form wrapping an envelope — the helper parses the string,
        // then digs into `data`.
        let envelope_str = r#"{"data":{"model":"x"},"error":null,"version":1}"#;
        let value = serde_json::Value::String(envelope_str.to_string());
        let inner = unwrap_dispatch_payload(&value, "X").unwrap();
        assert_eq!(inner, serde_json::json!({"model": "x"}));
    }

    #[test]
    fn unwrap_dispatch_payload_parses_string_bare_payload() {
        let bare_str = r#"{"model":"x","n":1}"#;
        let value = serde_json::Value::String(bare_str.to_string());
        let inner = unwrap_dispatch_payload(&value, "X").unwrap();
        assert_eq!(inner, serde_json::json!({"model": "x", "n": 1}));
    }

    /// `daemon_status` happy-path round-trip against a mock listener.
    /// Mirrors `daemon_ping_mock_round_trip` so a drift in either the envelope
    /// shape or the WatchSnapshot fields surfaces here.
    #[cfg(unix)]
    #[test]
    fn daemon_status_mock_round_trip() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let cqs_dir = dir.path().join(".cqs");
        std::fs::create_dir_all(&cqs_dir).unwrap();

        // Thread-local override avoids the unsafe set_var race that hangs CI.
        set_socket_dir_override_for_test(Some(dir.path().to_path_buf()));

        let sock_path = daemon_socket_path(&cqs_dir);
        let listener = UnixListener::bind(&sock_path).unwrap();

        // Mock the production envelope shape: `output` is the parsed
        // dispatch envelope object, not a JSON string.
        let snap = crate::watch_status::WatchSnapshot {
            state: crate::watch_status::FreshnessState::Stale,
            modified_files: 4,
            pending_notes: true,
            rebuild_in_flight: false,
            delta_saturated: false,
            incremental_count: 17,
            dropped_this_cycle: 0,
            last_event_unix_secs: 1_734_120_488,
            last_synced_at: Some(1_734_120_000),
            snapshot_at: Some(1_734_120_500),
            active_slot: None,
            ops: None,
        };
        let inner_envelope = serde_json::json!({
            "data": serde_json::to_value(&snap).unwrap(),
            "error": null,
            "version": 1,
        });
        let outer_envelope = serde_json::json!({
            "status": "ok",
            "output": inner_envelope,
        });
        let outer_str = outer_envelope.to_string();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request_line = String::new();
            BufReader::new(&stream)
                .read_line(&mut request_line)
                .unwrap();
            assert!(
                request_line.contains("\"command\":\"status\""),
                "expected status request, got: {request_line}"
            );
            writeln!(stream, "{outer_str}").unwrap();
            stream.flush().unwrap();
        });

        let result = daemon_status(&cqs_dir);
        handle.join().unwrap();
        let _ = std::fs::remove_file(&sock_path);

        set_socket_dir_override_for_test(None);

        let resp = result.expect("daemon_status should succeed against mock");
        assert_eq!(resp.state, crate::watch_status::FreshnessState::Stale);
        assert_eq!(resp.modified_files, 4);
        assert!(resp.pending_notes);
        assert_eq!(resp.incremental_count, 17);
        assert_eq!(resp.last_synced_at, Some(1_734_120_000));
    }

    /// `daemon_status` surfaces a friendly error (not a panic / generic IO)
    /// when no daemon socket exists. Mirrors the `daemon_ping` parity test.
    #[cfg(unix)]
    #[test]
    fn daemon_status_errors_without_socket() {
        let dir = tempfile::tempdir().unwrap();
        let cqs_dir = dir.path().join(".cqs");
        std::fs::create_dir_all(&cqs_dir).unwrap();
        let result = daemon_status(&cqs_dir);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, DaemonRpcError::SocketMissing(_)),
            "expected SocketMissing variant, got: {err:?}"
        );
        let msg = err.as_message();
        assert!(
            msg.contains("no daemon running"),
            "expected friendly error, got: {msg}"
        );
    }

    /// `daemon_reconcile` happy-path round-trip against a mock listener.
    /// Asserts the wire shape of the request (command name + flag-style args)
    /// and the response deserialization.
    #[cfg(unix)]
    #[test]
    fn daemon_reconcile_mock_round_trip() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let cqs_dir = dir.path().join(".cqs");
        std::fs::create_dir_all(&cqs_dir).unwrap();

        // Thread-local override avoids the unsafe set_var race that hangs CI.
        set_socket_dir_override_for_test(Some(dir.path().to_path_buf()));

        let sock_path = daemon_socket_path(&cqs_dir);
        let listener = UnixListener::bind(&sock_path).unwrap();

        // Server response: dispatch envelope mirroring what
        // `dispatch_reconcile` would emit. Ok(...) implies the signal queued —
        // no dedicated field.
        let inner_envelope = serde_json::json!({
            "data": {
                "was_pending": false,
                "hook": "post-checkout",
                "args": ["abc123", "def456", "1"],
            },
            "error": null,
            "version": 1,
        });
        let outer_envelope = serde_json::json!({
            "status": "ok",
            "output": inner_envelope,
        });
        let outer_str = outer_envelope.to_string();

        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request_line = String::new();
            BufReader::new(&stream)
                .read_line(&mut request_line)
                .unwrap();
            assert!(
                request_line.contains("\"command\":\"reconcile\""),
                "expected reconcile request, got: {request_line}"
            );
            // The hook + args ride along as flag-style batch args.
            assert!(
                request_line.contains("\"--hook\""),
                "expected --hook flag in args, got: {request_line}"
            );
            assert!(
                request_line.contains("\"post-checkout\""),
                "expected hook name in args, got: {request_line}"
            );
            writeln!(stream, "{outer_str}").unwrap();
            stream.flush().unwrap();
        });

        let args: Vec<String> = ["abc123", "def456", "1"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let result = daemon_reconcile(&cqs_dir, Some("post-checkout"), &args);
        handle.join().unwrap();
        let _ = std::fs::remove_file(&sock_path);

        set_socket_dir_override_for_test(None);

        let resp = result.expect("daemon_reconcile should succeed against mock");
        assert!(!resp.was_pending);
        assert_eq!(resp.hook.as_deref(), Some("post-checkout"));
        assert_eq!(resp.args, vec!["abc123", "def456", "1"]);
    }

    /// `daemon_reconcile` forwards UTF-8 hook args verbatim — emoji, accented
    /// characters, and zero-width characters must round-trip through the JSON
    /// envelope without mangling. String handling that breaks on non-ASCII
    /// typically trips on multi-byte boundaries inside `BufRead::read_line`.
    #[cfg(unix)]
    #[test]
    fn daemon_reconcile_forwards_unicode_args() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;
        use std::sync::{Arc, Mutex};

        let dir = tempfile::tempdir().unwrap();
        let cqs_dir = dir.path().join(".cqs");
        std::fs::create_dir_all(&cqs_dir).unwrap();

        // Thread-local override avoids the unsafe set_var race that hangs CI.
        set_socket_dir_override_for_test(Some(dir.path().to_path_buf()));

        let sock_path = daemon_socket_path(&cqs_dir);
        let listener = UnixListener::bind(&sock_path).unwrap();

        let captured: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let captured_for_thread = captured.clone();

        let inner_envelope = serde_json::json!({
            "data": {
                "was_pending": false,
                "hook": "post-merge",
                "args": ["mañana", "🚀", "café"],
            },
            "error": null,
            "version": 1,
        });
        let outer_envelope = serde_json::json!({
            "status": "ok",
            "output": inner_envelope,
        });
        let outer_str = outer_envelope.to_string();

        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request_line = String::new();
            BufReader::new(&stream)
                .read_line(&mut request_line)
                .unwrap();
            *captured_for_thread.lock().unwrap() = Some(request_line.clone());
            writeln!(stream, "{outer_str}").unwrap();
            stream.flush().unwrap();
        });

        let args: Vec<String> = vec!["mañana".to_string(), "🚀".to_string(), "café".to_string()];
        let result = daemon_reconcile(&cqs_dir, Some("post-merge"), &args);
        handle.join().unwrap();
        let _ = std::fs::remove_file(&sock_path);

        set_socket_dir_override_for_test(None);

        let resp = result.expect("daemon_reconcile should succeed against mock");
        assert_eq!(resp.hook.as_deref(), Some("post-merge"));
        assert_eq!(resp.args, vec!["mañana", "🚀", "café"]);

        // Captured request line preserves UTF-8 bytes verbatim. JSON
        // escaping may render emoji as `🚀` surrogate pairs;
        // accept either the raw or escaped form.
        let req = captured
            .lock()
            .unwrap()
            .clone()
            .expect("server thread should have captured the request");
        assert!(
            req.contains("mañana") || req.contains("ma\\u00f1ana"),
            "request must preserve accented characters, got: {req}"
        );
        assert!(
            req.contains("café") || req.contains("caf\\u00e9"),
            "request must preserve accented characters, got: {req}"
        );
        // The emoji 🚀 is U+1F680, encoded either raw (4 UTF-8 bytes) or
        // as a surrogate pair `🚀` in serde_json escape mode.
        assert!(
            req.contains('🚀') || req.contains("\\uD83D\\uDE80"),
            "request must preserve emoji, got: {req}"
        );
    }

    /// `daemon_reconcile` surfaces a friendly error when no daemon socket
    /// exists. The CLI surface treats this as a `.cqs/.dirty` fallback
    /// trigger — the error string must be matchable.
    #[cfg(unix)]
    #[test]
    fn daemon_reconcile_errors_without_socket() {
        let dir = tempfile::tempdir().unwrap();
        let cqs_dir = dir.path().join(".cqs");
        std::fs::create_dir_all(&cqs_dir).unwrap();
        let args: Vec<String> = Vec::new();
        let result = daemon_reconcile(&cqs_dir, Some("post-merge"), &args);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, DaemonRpcError::SocketMissing(_)),
            "expected SocketMissing variant, got: {err:?}"
        );
        let msg = err.as_message();
        assert!(
            msg.contains("no daemon running"),
            "expected friendly error, got: {msg}"
        );
    }

    /// `wait_for_fresh` returns `Fresh` immediately when the first poll already
    /// reports fresh. Pins the no-cost-when-fresh promise — agents that never
    /// have a stale tree don't pay latency.
    #[cfg(unix)]
    #[test]
    fn wait_for_fresh_returns_fresh_on_first_poll() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let cqs_dir = dir.path().join(".cqs");
        std::fs::create_dir_all(&cqs_dir).unwrap();

        // Thread-local override avoids the unsafe set_var race that hangs CI.
        set_socket_dir_override_for_test(Some(dir.path().to_path_buf()));

        let sock_path = daemon_socket_path(&cqs_dir);
        let listener = UnixListener::bind(&sock_path).unwrap();

        let snap = crate::watch_status::WatchSnapshot {
            state: crate::watch_status::FreshnessState::Fresh,
            modified_files: 0,
            pending_notes: false,
            rebuild_in_flight: false,
            delta_saturated: false,
            incremental_count: 1234,
            dropped_this_cycle: 0,
            last_event_unix_secs: 1_734_120_470,
            last_synced_at: Some(1_734_120_000),
            snapshot_at: Some(1_734_120_500),
            active_slot: None,
            ops: None,
        };
        let inner_envelope = serde_json::json!({
            "data": serde_json::to_value(&snap).unwrap(),
            "error": null,
            "version": 1,
        });
        let outer_envelope = serde_json::json!({"status": "ok", "output": inner_envelope});
        let outer_str = outer_envelope.to_string();

        let handle = std::thread::spawn(move || {
            // Single accept — fresh on first poll terminates the loop.
            let (mut stream, _) = listener.accept().unwrap();
            let mut request_line = String::new();
            BufReader::new(&stream)
                .read_line(&mut request_line)
                .unwrap();
            writeln!(stream, "{outer_str}").unwrap();
            stream.flush().unwrap();
        });

        let result = wait_for_fresh(&cqs_dir, 5);
        handle.join().unwrap();
        let _ = std::fs::remove_file(&sock_path);

        set_socket_dir_override_for_test(None);

        match result {
            FreshnessWait::Fresh(snap) => {
                assert_eq!(snap.state, crate::watch_status::FreshnessState::Fresh);
                assert_eq!(snap.incremental_count, 1234);
            }
            other => panic!("expected Fresh, got: {other:?}"),
        }
    }

    /// `wait_for_fresh` reports `Timeout` when the deadline expires before any
    /// successful poll. With `wait_secs = 0` the deadline-first guard fires
    /// immediately, returning `Timeout(unknown)` without a wasted round-trip.
    /// The `unknown` snapshot signals "we never got a real status" so the
    /// caller's bail message can adapt.
    #[cfg(unix)]
    #[test]
    fn wait_for_fresh_returns_timeout_when_budget_expires() {
        let dir = tempfile::tempdir().unwrap();
        let cqs_dir = dir.path().join(".cqs");
        std::fs::create_dir_all(&cqs_dir).unwrap();

        // Thread-local override avoids the unsafe set_var race that hangs CI.
        set_socket_dir_override_for_test(Some(dir.path().to_path_buf()));

        // A bound socket so the helper sees a daemon socket file. We never
        // actually accept — the deadline-first guard returns before the
        // first daemon_status call.
        let sock_path = daemon_socket_path(&cqs_dir);
        let _listener = std::os::unix::net::UnixListener::bind(&sock_path).unwrap();

        let result = wait_for_fresh(&cqs_dir, 0);
        let _ = std::fs::remove_file(&sock_path);

        set_socket_dir_override_for_test(None);

        match result {
            FreshnessWait::Timeout(snap) => {
                // Deadline-first: budget=0 returns without a round-trip, so the
                // snapshot is the synthetic "unknown".
                assert_eq!(snap.state, crate::watch_status::FreshnessState::Unknown);
            }
            other => panic!("expected Timeout, got: {other:?}"),
        }
    }

    /// `wait_for_fresh` returns `NoDaemon` without a socket. No mock listener —
    /// the helper short-circuits on `daemon_status`'s pre-flight existence
    /// check. Points the socket dir at a fresh tempdir so the helper sees no
    /// socket regardless of any host daemon.
    ///
    /// Uses `wait_secs = 5` so we reach `daemon_status` and get the
    /// SocketMissing → NoDaemon path; `wait_secs = 0` would short-circuit on
    /// the deadline-first guard and return `Timeout(unknown)` without ever
    /// asking the daemon.
    #[cfg(unix)]
    #[test]
    fn wait_for_fresh_returns_no_daemon_without_socket() {
        let dir = tempfile::tempdir().unwrap();
        let cqs_dir = dir.path().join(".cqs");
        std::fs::create_dir_all(&cqs_dir).unwrap();

        // Thread-local override avoids the unsafe set_var race that hangs CI.
        set_socket_dir_override_for_test(Some(dir.path().to_path_buf()));

        let result = wait_for_fresh(&cqs_dir, 5);

        set_socket_dir_override_for_test(None);

        match result {
            FreshnessWait::NoDaemon(msg) => {
                assert!(
                    msg.contains("no daemon running"),
                    "expected friendly error, got: {msg}"
                );
            }
            other => panic!("expected NoDaemon, got: {other:?}"),
        }
    }

    /// `wait_for_fresh` is a single round-trip. When the mock daemon replies
    /// with a Stale snapshot (i.e. the daemon's `dispatch_wait_fresh` parked,
    /// hit its deadline, and returned the still-stale snapshot), the helper
    /// must surface `Timeout(snap)` so the eval gate's bail message can quote
    /// the `modified_files` / `pending_notes` counters.
    #[cfg(unix)]
    #[test]
    fn wait_for_fresh_returns_timeout_with_stale_snapshot_from_server() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let cqs_dir = dir.path().join(".cqs");
        std::fs::create_dir_all(&cqs_dir).unwrap();
        set_socket_dir_override_for_test(Some(dir.path().to_path_buf()));

        let sock_path = daemon_socket_path(&cqs_dir);
        let listener = UnixListener::bind(&sock_path).unwrap();

        let stale_envelope = {
            let snap = crate::watch_status::WatchSnapshot {
                state: crate::watch_status::FreshnessState::Stale,
                modified_files: 7,
                pending_notes: false,
                rebuild_in_flight: false,
                delta_saturated: false,
                incremental_count: 0,
                dropped_this_cycle: 0,
                last_event_unix_secs: 1_734_120_488,
                last_synced_at: Some(1_734_120_000),
                snapshot_at: Some(1_734_120_500),
                active_slot: None,
                ops: None,
            };
            let inner = serde_json::json!({
                "data": serde_json::to_value(&snap).unwrap(),
                "error": null,
                "version": 1,
            });
            serde_json::json!({"status": "ok", "output": inner}).to_string()
        };

        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut req = String::new();
            BufReader::new(&stream).read_line(&mut req).unwrap();
            // Mock acts as the post-deadline server: replies with the
            // still-stale snapshot. No artificial sleep — the helper
            // doesn't depend on wall-clock for this path.
            writeln!(stream, "{stale_envelope}").unwrap();
            stream.flush().unwrap();
        });

        let result = wait_for_fresh(&cqs_dir, 5);
        handle.join().unwrap();
        let _ = std::fs::remove_file(&sock_path);
        set_socket_dir_override_for_test(None);

        match result {
            FreshnessWait::Timeout(snap) => {
                assert_eq!(snap.state, crate::watch_status::FreshnessState::Stale);
                assert_eq!(snap.modified_files, 7);
            }
            other => panic!("expected Timeout(stale), got: {other:?}"),
        }
    }

    /// `wait_for_fresh` returns `Transport(_)` when the daemon socket exists
    /// but the daemon dies mid-call. The listener accepts once, drops without
    /// writing a response — the helper's read times out and surfaces as
    /// Transport so the eval gate's advice points at journalctl rather than
    /// "start the daemon."
    #[cfg(unix)]
    #[test]
    fn wait_for_fresh_returns_transport_when_daemon_drops_mid_call() {
        use std::os::unix::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let cqs_dir = dir.path().join(".cqs");
        std::fs::create_dir_all(&cqs_dir).unwrap();
        set_socket_dir_override_for_test(Some(dir.path().to_path_buf()));

        let sock_path = daemon_socket_path(&cqs_dir);
        let listener = UnixListener::bind(&sock_path).unwrap();

        // Listener thread: accept once and drop both stream and listener
        // without writing. The helper's read times out → Transport.
        let handle = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            drop(stream);
            drop(listener);
        });

        // Use wait_secs=1 so the test wraps up quickly. Client-side
        // timeout is 1 + 5 = 6 s; the read times out at the per-call
        // timeout.
        let result = wait_for_fresh(&cqs_dir, 1);
        handle.join().unwrap();
        let _ = std::fs::remove_file(&sock_path);
        set_socket_dir_override_for_test(None);

        match result {
            FreshnessWait::Transport(_) => { /* expected */ }
            other => panic!("expected Transport after daemon drop, got: {other:?}"),
        }
    }

    /// `daemon_status` distinguishes a malformed envelope (`BadResponse`) from
    /// a missing socket (`SocketMissing`). The mock listener writes a truncated
    /// JSON line and closes — the helper must classify as BadResponse so
    /// callers like `wait_for_fresh` can route to "version skew" advice rather
    /// than "start a daemon".
    #[cfg(unix)]
    #[test]
    fn daemon_status_returns_bad_response_on_malformed_envelope() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let cqs_dir = dir.path().join(".cqs");
        std::fs::create_dir_all(&cqs_dir).unwrap();

        // Thread-local override avoids the unsafe set_var race that hangs CI.
        set_socket_dir_override_for_test(Some(dir.path().to_path_buf()));

        let sock_path = daemon_socket_path(&cqs_dir);
        let listener = UnixListener::bind(&sock_path).unwrap();

        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut req = String::new();
            BufReader::new(&stream).read_line(&mut req).unwrap();
            // Truncated envelope — opens an object then closes the line.
            writeln!(stream, r#"{{"status":"ok","output":"#).unwrap();
            stream.flush().unwrap();
        });

        let result = daemon_status(&cqs_dir);
        handle.join().unwrap();
        let _ = std::fs::remove_file(&sock_path);

        set_socket_dir_override_for_test(None);

        match result {
            Err(DaemonRpcError::BadResponse(msg)) => {
                assert!(
                    msg.contains("parse")
                        || msg.contains("envelope")
                        || msg.contains("missing")
                        || msg.contains("EOF")
                        || msg.contains("trailing"),
                    "expected envelope/parse-class message, got: {msg}"
                );
            }
            other => panic!("expected BadResponse on malformed envelope, got: {other:?}"),
        }
    }

    /// `daemon_socket_path` is deterministic across calls (BLAKE3 of the
    /// cqs_dir bytes). Pin the exact hash for a known input so a drift in the
    /// truncation length or hex formatting trips this test.
    #[cfg(unix)]
    #[test]
    fn daemon_socket_path_blake3_pinned() {
        use std::path::Path;
        set_socket_dir_override_for_test(Some(std::path::PathBuf::from("/tmp")));
        let p = daemon_socket_path(Path::new("/tmp/foo"));
        set_socket_dir_override_for_test(None);

        // Compute the expected hex independently so the test pins both
        // the algorithm choice (BLAKE3) and the truncation length (8 bytes
        // → 16 hex chars). If anyone swaps to SHA256 / changes the
        // truncation, this fails immediately.
        let expected_hash = blake3::hash(b"/tmp/foo");
        let expected_truncated = &expected_hash.as_bytes()[..8];
        let mut expected_hex = String::with_capacity(16);
        for b in expected_truncated {
            use std::fmt::Write as _;
            write!(expected_hex, "{:02x}", b).unwrap();
        }
        let expected = format!("/tmp/cqs-{expected_hex}.sock");
        assert_eq!(p.to_string_lossy(), expected);
        // Sanity: 16-char hex, fixed width — distinguishes from
        // DefaultHasher's variable-length unpadded output.
        assert_eq!(expected_hex.len(), 16, "BLAKE3 truncation must be 8 bytes");
    }

    // ===== daemon-side error-envelope handling =====
    //
    // A non-ok status with a missing or non-string `message` field is a
    // malformed envelope: it surfaces as a `BadResponse` naming the shape
    // mismatch rather than a placeholder `DaemonError`. Only a genuine
    // string `message` becomes a `DaemonError`.

    /// `{"status":"err"}` with no `message` field. The absence of the field
    /// is a malformed envelope, surfaced as a `BadResponse` that names the
    /// omission rather than echoing a placeholder that would render as the
    /// doubled `"daemon error: daemon error"`.
    #[cfg(unix)]
    #[test]
    fn daemon_status_handles_err_envelope_with_no_message() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let cqs_dir = dir.path().join(".cqs");
        std::fs::create_dir_all(&cqs_dir).unwrap();

        // Thread-local override avoids the unsafe set_var race that hangs CI.
        set_socket_dir_override_for_test(Some(dir.path().to_path_buf()));

        let sock_path = daemon_socket_path(&cqs_dir);
        let listener = UnixListener::bind(&sock_path).unwrap();

        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut req = String::new();
            BufReader::new(&stream).read_line(&mut req).unwrap();
            // Err envelope with no `message` field at all.
            writeln!(stream, r#"{{"status":"err"}}"#).unwrap();
            stream.flush().unwrap();
        });

        let result = daemon_status(&cqs_dir);
        handle.join().unwrap();
        let _ = std::fs::remove_file(&sock_path);

        set_socket_dir_override_for_test(None);

        match result {
            Err(DaemonRpcError::BadResponse(msg)) => {
                // The missing field is named, not papered over with a placeholder.
                assert!(
                    msg.contains("omitted the 'message' field"),
                    "BadResponse must name the missing message field, got: {msg}",
                );
                // The rendered wire-format string is no longer the doubled form.
                let rendered = DaemonRpcError::BadResponse(msg).as_message();
                assert!(
                    !rendered.contains("daemon error: daemon error"),
                    "rendering must not be the doubled placeholder, got: {rendered}",
                );
            }
            other => panic!("expected BadResponse naming the missing message, got: {other:?}",),
        }
    }

    /// `{"status":"err","message": 42}` (non-string message). A non-string
    /// `message` is a shape mismatch, surfaced as a `BadResponse` that carries
    /// the offending value rather than silently collapsing to a placeholder.
    #[cfg(unix)]
    #[test]
    fn daemon_status_handles_err_envelope_with_non_string_message() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let cqs_dir = dir.path().join(".cqs");
        std::fs::create_dir_all(&cqs_dir).unwrap();

        // Thread-local override avoids the unsafe set_var race that hangs CI.
        set_socket_dir_override_for_test(Some(dir.path().to_path_buf()));

        let sock_path = daemon_socket_path(&cqs_dir);
        let listener = UnixListener::bind(&sock_path).unwrap();

        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut req = String::new();
            BufReader::new(&stream).read_line(&mut req).unwrap();
            // Non-string message: integer 42.
            writeln!(stream, r#"{{"status":"err","message":42}}"#).unwrap();
            stream.flush().unwrap();
        });

        let result = daemon_status(&cqs_dir);
        handle.join().unwrap();
        let _ = std::fs::remove_file(&sock_path);

        set_socket_dir_override_for_test(None);

        match result {
            Err(DaemonRpcError::BadResponse(msg)) => {
                // The offending integer is surfaced, not dropped.
                assert!(
                    msg.contains("non-string 'message'") && msg.contains("42"),
                    "BadResponse must name the shape mismatch and carry the value, got: {msg}",
                );
            }
            other => panic!("expected BadResponse naming the non-string message, got: {other:?}",),
        }
    }

    /// The MCP relay's response read is sized from
    /// `CQS_DAEMON_MAX_RESPONSE_BYTES` (default 16 MiB) — NOT a hardcoded 1 MiB.
    /// When a valid-but-large response fills the cap before a newline arrives,
    /// the relay returns a DISTINCT `ResponseTooLarge` naming the limit and the
    /// override env var, instead of an opaque `BadResponse` parse failure that
    /// would make MCP strictly narrower than the CLI it fronts.
    #[cfg(unix)]
    #[test]
    #[serial_test::serial(daemon_response_cap_env)]
    fn json_args_response_over_cap_is_distinct_too_large_error() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let cqs_dir = dir.path().join(".cqs");
        std::fs::create_dir_all(&cqs_dir).unwrap();

        // Tiny cap so a small mock payload exceeds it. The override resolver
        // rejects 0 and garbage, so a positive small value is honored.
        std::env::set_var("CQS_DAEMON_MAX_RESPONSE_BYTES", "64");

        set_socket_dir_override_for_test(Some(dir.path().to_path_buf()));
        let sock_path = daemon_socket_path(&cqs_dir);
        let listener = UnixListener::bind(&sock_path).unwrap();

        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut req = String::new();
            BufReader::new(&stream).read_line(&mut req).unwrap();
            // Write WELL over the 64-byte cap with NO terminating newline before
            // the cap, so `read_line` fills the buffer and stops at the cap.
            let big = "x".repeat(4096);
            write!(stream, "{{\"status\":\"ok\",\"output\":\"{big}\"}}").unwrap();
            stream.flush().unwrap();
            // Hold the connection open briefly so the reader hits the cap rather
            // than EOF.
            std::thread::sleep(std::time::Duration::from_millis(50));
        });

        let result = daemon_json_args_request(&cqs_dir, "search", &serde_json::json!({}));
        handle.join().unwrap();
        let _ = std::fs::remove_file(&sock_path);
        set_socket_dir_override_for_test(None);
        std::env::remove_var("CQS_DAEMON_MAX_RESPONSE_BYTES");

        match result {
            Err(DaemonRpcError::ResponseTooLarge(msg)) => {
                assert!(
                    msg.contains("CQS_DAEMON_MAX_RESPONSE_BYTES"),
                    "the too-large error must name the override env var, got: {msg}"
                );
                assert!(
                    msg.contains("MiB"),
                    "the too-large error must name the limit, got: {msg}"
                );
            }
            other => panic!("expected ResponseTooLarge on an over-cap response, got: {other:?}"),
        }
    }

    /// A normal, under-cap response is NOT misclassified as too-large: the
    /// distinct error fires only when the buffer actually fills.
    #[cfg(unix)]
    #[test]
    #[serial_test::serial(daemon_response_cap_env)]
    fn json_args_response_under_cap_is_not_too_large() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let cqs_dir = dir.path().join(".cqs");
        std::fs::create_dir_all(&cqs_dir).unwrap();

        // Default cap (no override) is 16 MiB — a small payload is well under.
        std::env::remove_var("CQS_DAEMON_MAX_RESPONSE_BYTES");

        set_socket_dir_override_for_test(Some(dir.path().to_path_buf()));
        let sock_path = daemon_socket_path(&cqs_dir);
        let listener = UnixListener::bind(&sock_path).unwrap();

        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut req = String::new();
            BufReader::new(&stream).read_line(&mut req).unwrap();
            writeln!(stream, r#"{{"status":"ok","output":{{"data":[]}}}}"#).unwrap();
            stream.flush().unwrap();
        });

        let result = daemon_json_args_request(&cqs_dir, "search", &serde_json::json!({}));
        handle.join().unwrap();
        let _ = std::fs::remove_file(&sock_path);
        set_socket_dir_override_for_test(None);

        // A normal small response succeeds (returns the full envelope), and in
        // no case is it classified as ResponseTooLarge.
        assert!(
            !matches!(result, Err(DaemonRpcError::ResponseTooLarge(_))),
            "an under-cap response must never be classified too-large, got: {result:?}"
        );
        assert!(
            result.is_ok(),
            "a well-formed under-cap response must parse"
        );
    }

    /// Boundary: a valid response that is EXACTLY the cap in bytes (its
    /// terminating newline included) is accepted, not mistaken for a truncated
    /// one. The cap-hit test reads one byte past the cap and fires only on
    /// `> cap`, so an exact-cap line (which `read_line` returns as `cap` bytes,
    /// newline and all) parses cleanly. Under the prior `== cap` test this
    /// false-tripped `ResponseTooLarge`.
    #[cfg(unix)]
    #[test]
    #[serial_test::serial(daemon_response_cap_env)]
    fn json_args_response_exactly_at_cap_is_accepted() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let cqs_dir = dir.path().join(".cqs");
        std::fs::create_dir_all(&cqs_dir).unwrap();

        let cap: usize = 512;
        std::env::set_var("CQS_DAEMON_MAX_RESPONSE_BYTES", cap.to_string());

        set_socket_dir_override_for_test(Some(dir.path().to_path_buf()));
        let sock_path = daemon_socket_path(&cqs_dir);
        let listener = UnixListener::bind(&sock_path).unwrap();

        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut req = String::new();
            BufReader::new(&stream).read_line(&mut req).unwrap();
            // A valid envelope padded so the line + its trailing newline is
            // EXACTLY `cap` bytes. `writeln!` adds the newline, so the pre-newline
            // line must be `cap - 1` bytes.
            let wrapper = r#"{"status":"ok","output":{"d":""}}"#;
            let pad = cap - 1 - wrapper.len();
            let big = "x".repeat(pad);
            let line = format!(r#"{{"status":"ok","output":{{"d":"{big}"}}}}"#);
            assert_eq!(
                line.len(),
                cap - 1,
                "line must be exactly cap-1 pre-newline"
            );
            writeln!(stream, "{line}").unwrap();
            stream.flush().unwrap();
        });

        let result = daemon_json_args_request(&cqs_dir, "search", &serde_json::json!({}));
        handle.join().unwrap();
        let _ = std::fs::remove_file(&sock_path);
        set_socket_dir_override_for_test(None);
        std::env::remove_var("CQS_DAEMON_MAX_RESPONSE_BYTES");

        assert!(
            !matches!(result, Err(DaemonRpcError::ResponseTooLarge(_))),
            "an exactly-at-cap valid response must NOT be classified too-large, got: {result:?}"
        );
        assert!(
            result.is_ok(),
            "an exactly-at-cap well-formed response must parse, got: {result:?}"
        );
    }

    /// Property (cap boundary): for a generated total line length `L` (the
    /// newline included) and a fixed cap `C`, the daemon-response read accepts
    /// the line iff `L <= C` and rejects with `ResponseTooLarge` iff `L > C`.
    ///
    /// The fixed `json_args_response_exactly_at_cap_is_accepted` example pins ONE
    /// point (`L == C`). This sweeps `L` across `[C-3, C+3]` — the exact window
    /// where the prior `== cap` test mis-fired (it rejected `L == C`, the very
    /// case a valid full-cap payload occupies). A regression to a `>= cap` or
    /// `== cap` check falsifies at `L == C`; an off-by-one in the `take(C+1)`
    /// budget falsifies at `L == C+1`.
    ///
    /// Each case sends a JSON envelope padded to exactly `L` bytes including the
    /// trailing newline, so the only variable is length-vs-cap, never validity.
    #[cfg(unix)]
    #[test]
    #[serial_test::serial(daemon_response_cap_env)]
    fn prop_cap_accepts_iff_len_le_cap() {
        use proptest::prelude::*;
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        // Fixed cap; the wrapper below is well under it so padding always has room.
        const CAP: usize = 512;
        // A valid envelope whose `d` field absorbs the padding. Pre-newline this
        // is `WRAPPER.len() + pad` bytes; `writeln!` adds 1 for the newline, so
        // the total line length is `WRAPPER.len() + pad + 1`.
        const WRAPPER: &str = r#"{"status":"ok","output":{"d":""}}"#;

        // One round-trip for a target TOTAL line length `total` (newline incl.).
        // Returns the classification: true = accepted (Ok), false = rejected
        // (ResponseTooLarge). Any other error is a test-harness fault → panic.
        fn run_one(cap: usize, total: usize) -> bool {
            let dir = tempfile::tempdir().unwrap();
            let cqs_dir = dir.path().join(".cqs");
            std::fs::create_dir_all(&cqs_dir).unwrap();
            std::env::set_var("CQS_DAEMON_MAX_RESPONSE_BYTES", cap.to_string());
            set_socket_dir_override_for_test(Some(dir.path().to_path_buf()));
            let sock_path = daemon_socket_path(&cqs_dir);
            let listener = UnixListener::bind(&sock_path).unwrap();

            // Pre-newline length = total - 1; pad the `d` field to hit it exactly.
            let pre_newline = total - 1;
            let pad = pre_newline - WRAPPER.len();
            let handle = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut req = String::new();
                let _ = BufReader::new(&stream).read_line(&mut req);
                let big = "x".repeat(pad);
                let line = format!(r#"{{"status":"ok","output":{{"d":"{big}"}}}}"#);
                assert_eq!(
                    line.len(),
                    pre_newline,
                    "padding math: line must be total-1"
                );
                // `writeln!` appends the newline → on-wire line is `total` bytes.
                // For an OVER-cap payload the client reads only `cap + 1` bytes
                // then returns `ResponseTooLarge` and drops the stream, so this
                // write can hit a broken pipe — that is the expected shape, not a
                // harness fault. Ignore write/flush errors; the assertion is about
                // the CLIENT's classification, not a clean server-side write.
                let _ = writeln!(stream, "{line}");
                let _ = stream.flush();
            });

            let result = daemon_json_args_request(&cqs_dir, "search", &serde_json::json!({}));
            let _ = handle.join();
            let _ = std::fs::remove_file(&sock_path);
            set_socket_dir_override_for_test(None);
            std::env::remove_var("CQS_DAEMON_MAX_RESPONSE_BYTES");

            match result {
                Ok(_) => true,
                Err(DaemonRpcError::ResponseTooLarge(_)) => false,
                Err(other) => panic!("unexpected error for total={total} cap={cap}: {other:?}"),
            }
        }

        // Sweep the boundary window [CAP-3, CAP+3]. Lower bound stays above the
        // wrapper length so padding is non-negative.
        let mut runner = proptest::test_runner::TestRunner::deterministic();
        let strat = (CAP - 3)..=(CAP + 3);
        runner
            .run(&strat, |total| {
                let accepted = run_one(CAP, total);
                let expected = total <= CAP;
                prop_assert_eq!(
                    accepted,
                    expected,
                    "len={} cap={}: accepted={} but len<=cap is {}",
                    total,
                    CAP,
                    accepted,
                    expected
                );
                Ok(())
            })
            .unwrap();
    }

    // ── socket parent-dir hardening ───────────────────────────────────

    /// Creating a fresh socket parent dir produces an empty 0o700 directory.
    #[cfg(unix)]
    #[test]
    fn ensure_socket_parent_dir_creates_private_when_missing() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::TempDir::new().unwrap();
        let parent = tmp.path().join("subdir");
        let sock = parent.join("cqs-test.sock");
        assert!(!parent.exists());
        ensure_socket_parent_dir(&sock).unwrap();
        let md = std::fs::metadata(&parent).unwrap();
        assert!(md.is_dir());
        let mode = md.permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o700,
            "newly-created socket parent dir must be 0o700, got {:o}",
            mode
        );
    }

    /// An existing dir with looser mode gets tightened to 0o700.
    #[cfg(unix)]
    #[test]
    fn ensure_socket_parent_dir_tightens_loose_existing_mode() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::TempDir::new().unwrap();
        let parent = tmp.path().join("loose");
        std::fs::create_dir(&parent).unwrap();
        // Set 0o755 (group + other read+exec) — the threat model.
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();
        let sock = parent.join("cqs-test.sock");
        ensure_socket_parent_dir(&sock).unwrap();
        let mode = std::fs::metadata(&parent).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o700,
            "loose-mode parent dir must be tightened to 0o700, got {:o}",
            mode
        );
    }

    /// A regular file squatting on the parent path is fatal — the daemon
    /// must refuse to start rather than silently fail on the bind side.
    #[cfg(unix)]
    #[test]
    fn ensure_socket_parent_dir_refuses_when_parent_is_a_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let parent = tmp.path().join("not-a-dir");
        // Create a regular file at the parent path.
        std::fs::write(&parent, b"squatting").unwrap();
        let sock = parent.join("cqs-test.sock");
        let err = ensure_socket_parent_dir(&sock).expect_err("must reject file-at-parent-path");
        let msg = err.to_string();
        assert!(
            msg.contains("not a directory") || msg.contains("SEC-V1.36-10"),
            "error must name the bug ID or shape, got: {msg}"
        );
    }

    /// A symlink at the parent path is fatal — symlinks across the
    /// socket dir are an attack vector (a hostile user planting a
    /// symlink to a dir they control).
    #[cfg(unix)]
    #[test]
    fn ensure_socket_parent_dir_refuses_when_parent_is_a_symlink() {
        let tmp = tempfile::TempDir::new().unwrap();
        let target = tmp.path().join("real");
        std::fs::create_dir(&target).unwrap();
        let parent = tmp.path().join("link-to-real");
        std::os::unix::fs::symlink(&target, &parent).unwrap();
        let sock = parent.join("cqs-test.sock");
        let err = ensure_socket_parent_dir(&sock).expect_err("must reject symlink-at-parent");
        let msg = err.to_string();
        assert!(
            msg.contains("symlink") || msg.contains("SEC-V1.36-10"),
            "error must name the bug ID or shape, got: {msg}"
        );
    }

    /// `unwrap_dispatch_payload` surfaces a non-null `error` field rather than
    /// returning the accompanying (null) `data` payload. An envelope
    /// advertising `{"data": null, "error": "internal"}` returns `Err(_)`
    /// carrying the error text instead of silently dropping it.
    #[test]
    fn unwrap_dispatch_payload_surfaces_envelope_error_over_null_data() {
        let v = serde_json::json!({"data": null, "error": "internal", "version": 1});
        let result = unwrap_dispatch_payload(&v, "TestType");
        // A non-null `error` alongside `data: null` is a handler failure.
        let err = result.expect_err("non-null error field must surface as Err");
        assert!(
            err.contains("internal"),
            "surfaced error must carry the envelope's error text, got: {err}",
        );
    }

    #[test]
    fn classify_slim_data_envelope() {
        let v = serde_json::json!({"data": {"x": 1}});
        match classify_slim_envelope(&v) {
            Some(SlimEnvelope::Data { payload, meta }) => {
                assert_eq!(payload, &serde_json::json!({"x": 1}));
                assert!(meta.is_none());
            }
            other => panic!("expected Data, got {:?}", other.is_some()),
        }
        let v = serde_json::json!({"data": [1, 2], "_meta": {"worktree_stale": true}});
        match classify_slim_envelope(&v) {
            Some(SlimEnvelope::Data { payload, meta }) => {
                assert_eq!(payload, &serde_json::json!([1, 2]));
                assert!(meta.is_some());
            }
            _ => panic!("expected Data with meta"),
        }
    }

    #[test]
    fn classify_slim_error_envelope() {
        let v = serde_json::json!({"error": {"code": "not_found", "message": "nope"}});
        match classify_slim_envelope(&v) {
            Some(SlimEnvelope::Error { code, message }) => {
                assert_eq!(code, "not_found");
                assert_eq!(message, "nope");
            }
            _ => panic!("expected Error"),
        }
    }

    #[test]
    fn classify_rejects_non_slim_shapes() {
        // Full v1 envelope (has version) passes through untouched.
        assert!(classify_slim_envelope(&serde_json::json!(
            {"data": null, "error": null, "version": 1}
        ))
        .is_none());
        // Payload that merely contains a data field among other keys.
        assert!(classify_slim_envelope(&serde_json::json!(
            {"data": 1, "other": 2}
        ))
        .is_none());
        // Arrays, scalars, empty objects.
        assert!(classify_slim_envelope(&serde_json::json!([1])).is_none());
        assert!(classify_slim_envelope(&serde_json::json!(7)).is_none());
        assert!(classify_slim_envelope(&serde_json::json!({})).is_none());
        // data AND error together is not the slim contract.
        assert!(classify_slim_envelope(&serde_json::json!(
            {"data": 1, "error": {"code": "x", "message": "y"}}
        ))
        .is_none());
    }
}
