//! `cqs slot` subcommand — list / create / promote / remove / active.
//!
//! Spec §Slot commands: project-level named slots living under
//! `.cqs/slots/<name>/`. See `docs/plans/2026-04-24-embeddings-cache-and-slots.md`
//! for the design. Migration from a legacy `.cqs/index.db` runs at the top of
//! `dispatch::run_with` (see `src/cli/dispatch.rs`).

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use clap::Subcommand;
use colored::Colorize;

use cqs::slot::{
    acquire_slots_lock, active_slot_path, list_slots, read_active_slot, slot_dir,
    validate_slot_name, write_active_slot, write_slot_model, DEFAULT_SLOT,
};

use crate::cli::config::find_project_root;
use crate::cli::definitions::TextJsonArgs;
use crate::cli::Cli;

/// Summary row for `cqs slot list`.
#[derive(Debug, serde::Serialize)]
pub(crate) struct SlotListEntry {
    pub name: String,
    pub active: bool,
    /// `true` if `<slot_dir>/index.db` is present. False slots are valid
    /// "create-and-not-yet-indexed" states.
    pub indexed: bool,
    /// Number of chunks in the slot's index. `None` if the index is missing
    /// or unreadable; the slot still shows up in the list.
    pub chunks: Option<u64>,
    /// Embedding model recorded in the slot's metadata (e.g.
    /// `BAAI/bge-large-en-v1.5`). `None` for un-indexed slots.
    pub model: Option<String>,
    /// Embedding dimension recorded in the slot's metadata. `None` for
    /// un-indexed slots.
    pub dim: Option<u64>,
    /// Slot dir absolute path.
    pub path: String,
}

#[derive(Subcommand, Clone, Debug)]
pub(crate) enum SlotCommand {
    /// List all slots, marking the active one
    List {
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Create a new empty slot directory
    Create {
        /// Slot name (lowercase a-z, 0-9, `_`, `-`; max 32 chars)
        name: String,
        /// Embedding model preset or HF repo id (e.g. `bge-large`, `e5-base`,
        /// `BAAI/bge-large-en-v1.5`). Validated against `ModelConfig::resolve`.
        #[arg(long)]
        model: Option<String>,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Make a slot the active one (atomic pointer update)
    Promote {
        /// Slot name to promote
        name: String,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Remove a slot directory and all its files
    Remove {
        /// Slot name to remove
        name: String,
        /// Allow removing the active slot if at least one other slot exists
        #[arg(long)]
        force: bool,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Print the active slot name
    Active {
        #[command(flatten)]
        output: TextJsonArgs,
    },
}

pub(crate) fn cmd_slot(cli: &Cli, subcmd: &SlotCommand) -> Result<()> {
    let _span = tracing::info_span!("cmd_slot").entered();

    // P2.13: surface a hard error when `--slot` is passed at the global level.
    // The global flag is a path-resolution input for *project-scoped* commands
    // (search, index, …), but `cqs slot <subcmd>` already takes its target
    // slot positionally. Silently ignoring the global meant `cqs slot create
    // foo --slot bar` "succeeded" while ignoring `bar`. Fail loudly instead.
    if cli.slot.is_some() {
        anyhow::bail!(
            "--slot has no effect on `cqs slot` subcommands (this command is project-scoped, slot targets are taken positionally)"
        );
    }

    let root = find_project_root();
    let project_cqs_dir = cqs::resolve_index_dir(&root);
    if !project_cqs_dir.exists() {
        anyhow::bail!(
            "No `.cqs/` directory found in {}. Run `cqs init && cqs index` first.",
            root.display()
        );
    }

    match subcmd {
        SlotCommand::List { output } => slot_list(&project_cqs_dir, cli.json || output.json),
        SlotCommand::Create {
            name,
            model,
            output,
        } => slot_create(
            &project_cqs_dir,
            name,
            model.as_deref(),
            cli.json || output.json,
        ),
        SlotCommand::Promote { name, output } => {
            slot_promote(&project_cqs_dir, name, cli.json || output.json)
        }
        SlotCommand::Remove {
            name,
            force,
            output,
        } => slot_remove(&project_cqs_dir, name, *force, cli.json || output.json),
        SlotCommand::Active { output } => slot_active(&project_cqs_dir, cli.json || output.json),
    }
}

fn slot_list(project_cqs_dir: &Path, json: bool) -> Result<()> {
    let _span = tracing::info_span!("slot_list").entered();
    let names = list_slots(project_cqs_dir)?;
    let active = read_active_slot(project_cqs_dir).unwrap_or_else(|| DEFAULT_SLOT.to_string());
    let entries: Vec<SlotListEntry> = names
        .into_iter()
        .map(|name| collect_slot_entry(project_cqs_dir, &name, &active))
        .collect();

    if json {
        let obj = serde_json::json!({
            "active": active,
            "slots": entries,
        });
        crate::cli::json_envelope::emit_json(&obj)?;
    } else if entries.is_empty() {
        println!("No slots found.");
        println!("Use `cqs slot create <name> --model <preset-or-hf>` to add one,");
        println!("or run `cqs index` to populate the default slot.");
    } else {
        for e in &entries {
            let mark = if e.active {
                "*".green().bold()
            } else {
                " ".normal()
            };
            let chunks = e
                .chunks
                .map(|n| n.to_string())
                .unwrap_or_else(|| "-".to_string());
            let model = e.model.as_deref().unwrap_or("-");
            let dim = e
                .dim
                .map(|d| d.to_string())
                .unwrap_or_else(|| "-".to_string());
            let status = if e.indexed {
                "ok".green().to_string()
            } else {
                "empty".yellow().to_string()
            };
            println!(
                "{} {:<20} chunks={:<8} model={:<28} dim={:<5} [{}]",
                mark, e.name, chunks, model, dim, status
            );
        }
        println!();
        println!("Active slot: {}", active);
    }
    Ok(())
}

/// Open the slot's `index.db` read-only (with a small footprint suitable for
/// dozens of slots) and pull its chunk count + model metadata. Best-effort —
/// listing should succeed even if one slot's DB is unreadable.
fn collect_slot_entry(project_cqs_dir: &Path, name: &str, active: &str) -> SlotListEntry {
    let dir = slot_dir(project_cqs_dir, name);
    let index_path = dir.join(cqs::INDEX_DB_FILENAME);
    let path_str = dir.display().to_string();
    if !index_path.exists() {
        return SlotListEntry {
            name: name.to_string(),
            active: name == active,
            indexed: false,
            chunks: None,
            model: None,
            dim: None,
            path: path_str,
        };
    }
    let (chunks, model, dim) = match cqs::Store::open_readonly_small(&index_path) {
        Ok(store) => {
            let count = store.chunk_count().ok();
            let model = store.stored_model_name();
            let dim = u64::try_from(store.dim()).ok();
            (count, model, dim)
        }
        Err(e) => {
            tracing::warn!(
                slot = name,
                error = %e,
                path = %index_path.display(),
                "Slot index read failed during listing"
            );
            (None, None, None)
        }
    };
    SlotListEntry {
        name: name.to_string(),
        active: name == active,
        indexed: true,
        chunks,
        model,
        dim,
        path: path_str,
    }
}

fn slot_create(project_cqs_dir: &Path, name: &str, model: Option<&str>, json: bool) -> Result<()> {
    let _span = tracing::info_span!("slot_create", name, model).entered();
    validate_slot_name(name)?;

    // P2.61 / P2.28: serialize lifecycle ops cross-process so two concurrent
    // `slot create foo` calls (or create+promote+remove against the same name)
    // can't interleave their read-validate-mutate steps and corrupt the active
    // pointer. Lock is released when this function returns.
    let _slots_lock = acquire_slots_lock(project_cqs_dir)?;

    let dir = slot_dir(project_cqs_dir, name);
    if dir.exists() {
        anyhow::bail!(
            "Slot '{}' already exists at {}. Either run `cqs index --slot {}` or `cqs slot remove {}` first.",
            name,
            dir.display(),
            name,
            name,
        );
    }
    fs::create_dir_all(&dir)?;

    // Validate the model now (preset or HF) so the user gets a fast error
    // before the next `cqs index` runs. The actual download happens later.
    // #1107: persist the user's intent in `slot.toml` so `cqs index --slot <name>`
    // picks it up automatically. We store the *user's input* (preset name like
    // `nomic-coderank`, or full HF repo like `BAAI/bge-large-en-v1.5`) — not the
    // resolved canonical repo — so future preset table additions don't shift
    // semantics out from under the user.
    let resolved_model: Option<String> = match model {
        Some(m) => {
            let cfg = cqs::embedder::ModelConfig::resolve(Some(m), None);
            write_slot_model(project_cqs_dir, name, m).map_err(anyhow::Error::from)?;
            Some(cfg.repo)
        }
        None => None,
    };

    if json {
        let obj = serde_json::json!({
            "name": name,
            "path": dir.display().to_string(),
            "model": resolved_model,
        });
        crate::cli::json_envelope::emit_json(&obj)?;
    } else {
        println!("Created slot '{}' at {}", name, dir.display());
        if let Some(ref m) = resolved_model {
            println!("Model resolved as: {m}");
        }
        println!("Next: `cqs index --slot {name}` to populate it.");
    }
    Ok(())
}

fn slot_promote(project_cqs_dir: &Path, name: &str, json: bool) -> Result<()> {
    let _span = tracing::info_span!("slot_promote", name).entered();
    validate_slot_name(name)?;

    // P2.61 / P2.28: see slot_create — exclusive lock prevents promote racing
    // a concurrent remove of the same slot.
    let _slots_lock = acquire_slots_lock(project_cqs_dir)?;

    let dir = slot_dir(project_cqs_dir, name);
    if !dir.exists() {
        let available = list_slots(project_cqs_dir).unwrap_or_default().join(", ");
        anyhow::bail!(
            "Slot '{}' does not exist. Available: [{}]. Create with: cqs slot create <name> --model <model-id>",
            name,
            available
        );
    }
    write_active_slot(project_cqs_dir, name)?;

    let warning = format!(
        "Active slot changed to '{}'. To serve queries from the new slot, restart the daemon:\n    systemctl --user restart cqs-watch",
        name
    );
    if json {
        let obj = serde_json::json!({
            "promoted": name,
            "warning": warning,
        });
        crate::cli::json_envelope::emit_json(&obj)?;
    } else {
        println!("Promoted slot '{name}' to active.");
        println!("{warning}");
    }
    Ok(())
}

fn slot_remove(project_cqs_dir: &Path, name: &str, force: bool, json: bool) -> Result<()> {
    let _span = tracing::info_span!("slot_remove", name, force).entered();
    validate_slot_name(name)?;

    // P2.61 / P2.28: hold exclusive lock across read_active_slot → list_slots
    // → remove_dir_all so a concurrent promote can't change `active_slot`
    // between steps and leave the system pointing at a deleted directory.
    let _slots_lock = acquire_slots_lock(project_cqs_dir)?;

    let dir = slot_dir(project_cqs_dir, name);
    if !dir.exists() {
        let available = list_slots(project_cqs_dir).unwrap_or_default().join(", ");
        anyhow::bail!(
            "Slot '{}' does not exist. Available: [{}].",
            name,
            available
        );
    }

    // DS-V1.30.1-D4 (#1232): refuse if a daemon is currently serving
    // this slot. The slots lock above pins out concurrent CLI promote /
    // remove calls but doesn't bind a long-lived `cqs watch --serve`
    // process — its `Store::open` against `slots/<name>/index.db`
    // holds open file descriptors that survive `fs::remove_dir_all`
    // on Linux, leaving WAL checkpoints persisting into a detached
    // directory tree that's reaped on daemon exit. Operators see no
    // error, lose hours of incremental rebuild work, and on WSL or
    // any non-overlay FS the unlink can partially fail and leave the
    // slot dir half-removed. Probe the daemon before unlinking; if
    // it's serving this slot, refuse (or downgrade to a warn under
    // `--force`).
    guard_against_active_daemon(project_cqs_dir, name, force)?;

    let active = read_active_slot(project_cqs_dir).unwrap_or_else(|| DEFAULT_SLOT.to_string());
    // P2.21: don't mask `list_slots` failure as "only slot remaining" — that
    // would falsely error on a transient FS hiccup. Surface the real cause so
    // operators can fix it (permission denied, dir gone, etc.) instead of
    // staring at a misleading "create another slot first" message.
    let mut all =
        list_slots(project_cqs_dir).context("Failed to list slots while validating remove")?;
    all.retain(|n| n != name);

    if name == active {
        if all.is_empty() {
            anyhow::bail!(
                "Refusing to remove the only remaining slot '{}'. Create another slot first.",
                name
            );
        }
        if !force {
            anyhow::bail!(
                "Slot '{}' is currently active. Promote a different slot first, or pass --force to auto-promote '{}' as the new active.",
                name,
                all[0]
            );
        }
        // Force: auto-promote the first remaining slot.
        write_active_slot(project_cqs_dir, &all[0])?;
        tracing::info!(promoted = %all[0], "auto-promoted new active slot after force remove");
    }

    fs::remove_dir_all(&dir)?;

    if json {
        let obj = serde_json::json!({
            "removed": name,
            "new_active": if name == active { Some(all[0].clone()) } else { None::<String> },
        });
        crate::cli::json_envelope::emit_json(&obj)?;
    } else {
        println!("Removed slot '{}'.", name);
        if name == active {
            println!("Active slot auto-promoted to '{}'.", all[0]);
        }
    }
    Ok(())
}

/// DS-V1.30.1-D4 (#1232): probe the daemon and refuse if it's serving
/// the slot the user is about to remove. Mirrors the existing
/// "this is the active slot" `--force` semantics: refusal becomes a
/// `tracing::warn!` when `force` is true.
///
/// Probe failure (no daemon, transport error, missing slot field) is
/// treated as "no daemon serving this slot" — the worst case is the
/// historical behavior, never a false positive. Probe takes ~5 ms when
/// the daemon is up and ~1 ms when the socket is missing, so the
/// extra round trip is invisible relative to the index-rebuild path.
///
/// Windows has no daemon today (`daemon_status` is unix-gated), so the
/// guard is a no-op there.
fn guard_against_active_daemon(project_cqs_dir: &Path, name: &str, force: bool) -> Result<()> {
    #[cfg(unix)]
    {
        let snap = match cqs::daemon_translate::daemon_status(project_cqs_dir) {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!(
                    error = %e,
                    "daemon probe failed during slot remove; assuming no daemon serving this slot"
                );
                return Ok(());
            }
        };
        if snap.active_slot.as_deref() != Some(name) {
            return Ok(());
        }
        if !force {
            anyhow::bail!(
                "daemon is currently serving slot '{name}'. \
                 Stop it first (e.g. `systemctl --user stop cqs-watch`, or kill the \
                 `cqs watch --serve` process) and re-run, or pass --force to override."
            );
        }
        tracing::warn!(
            slot = name,
            "removing slot while daemon is actively serving it (--force); \
             daemon will hold open file descriptors against the unlinked slot dir \
             until it exits — incremental rebuild work persisted after this point \
             may be silently lost"
        );
    }
    #[cfg(not(unix))]
    {
        let _ = (project_cqs_dir, name, force);
    }
    Ok(())
}

fn slot_active(project_cqs_dir: &Path, json: bool) -> Result<()> {
    let _span = tracing::info_span!("slot_active").entered();
    let resolved =
        cqs::slot::resolve_slot_name(None, project_cqs_dir).map_err(anyhow::Error::from)?;
    if json {
        let obj = serde_json::json!({
            "active": resolved.name,
            "source": resolved.source.as_str(),
            "active_slot_file": active_slot_path(project_cqs_dir).display().to_string(),
        });
        crate::cli::json_envelope::emit_json(&obj)?;
    } else {
        println!("{}", resolved.name);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cqs::slot::{slots_root, write_active_slot};
    use std::sync::Mutex;
    use tempfile::TempDir;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Helper: build a fresh project with a `.cqs/` and N empty slots.
    fn with_slots(slot_names: &[&str]) -> TempDir {
        let dir = TempDir::new().unwrap();
        let cqs = dir.path().join(".cqs");
        fs::create_dir_all(&cqs).unwrap();
        for n in slot_names {
            let d = slot_dir(&cqs, n);
            fs::create_dir_all(&d).unwrap();
        }
        dir
    }

    #[test]
    fn slot_create_rejects_invalid_name() {
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = with_slots(&[]);
        let cqs = tmp.path().join(".cqs");
        let r = slot_create(&cqs, "Bad-Name", None, true);
        assert!(r.is_err(), "uppercase should reject");
    }

    #[test]
    fn slot_create_rejects_reserved_name() {
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = with_slots(&[]);
        let cqs = tmp.path().join(".cqs");
        let r = slot_create(&cqs, "list", None, true);
        assert!(r.is_err());
        let r = slot_create(&cqs, "active", None, true);
        assert!(r.is_err());
    }

    #[test]
    fn slot_create_succeeds_then_dir_exists() {
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = with_slots(&[]);
        let cqs = tmp.path().join(".cqs");
        fs::create_dir_all(&cqs).unwrap();
        let r = slot_create(&cqs, "e5", None, true);
        assert!(r.is_ok(), "{:?}", r.err());
        assert!(slot_dir(&cqs, "e5").exists());
    }

    #[test]
    fn slot_create_with_model_persists_slot_toml() {
        // #1107: --model X must write `[embedding] model = "X"` so a later
        // `cqs index --slot <name>` (without --model) honors the user's intent.
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = with_slots(&[]);
        let cqs = tmp.path().join(".cqs");
        fs::create_dir_all(&cqs).unwrap();
        slot_create(&cqs, "coderank", Some("nomic-coderank"), true).unwrap();
        assert_eq!(
            cqs::slot::read_slot_model(&cqs, "coderank").as_deref(),
            Some("nomic-coderank")
        );
    }

    #[test]
    fn slot_create_without_model_leaves_slot_toml_absent() {
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = with_slots(&[]);
        let cqs = tmp.path().join(".cqs");
        fs::create_dir_all(&cqs).unwrap();
        slot_create(&cqs, "noflag", None, true).unwrap();
        assert!(cqs::slot::read_slot_model(&cqs, "noflag").is_none());
    }

    #[test]
    fn slot_create_refuses_existing() {
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = with_slots(&["dup"]);
        let cqs = tmp.path().join(".cqs");
        let r = slot_create(&cqs, "dup", None, true);
        assert!(r.is_err());
    }

    #[test]
    fn slot_promote_requires_existing_slot() {
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = with_slots(&["one"]);
        let cqs = tmp.path().join(".cqs");
        let r = slot_promote(&cqs, "missing", true);
        assert!(r.is_err());
    }

    #[test]
    fn slot_promote_updates_active_slot_file() {
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = with_slots(&["a", "b"]);
        let cqs = tmp.path().join(".cqs");
        write_active_slot(&cqs, "a").unwrap();
        slot_promote(&cqs, "b", true).unwrap();
        assert_eq!(read_active_slot(&cqs).as_deref(), Some("b"));
    }

    #[test]
    fn slot_remove_refuses_active_without_force() {
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = with_slots(&["active_one", "other"]);
        let cqs = tmp.path().join(".cqs");
        write_active_slot(&cqs, "active_one").unwrap();
        let r = slot_remove(&cqs, "active_one", false, true);
        assert!(r.is_err());
    }

    #[test]
    fn slot_remove_with_force_promotes_other() {
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = with_slots(&["a", "b"]);
        let cqs = tmp.path().join(".cqs");
        write_active_slot(&cqs, "a").unwrap();
        slot_remove(&cqs, "a", true, true).unwrap();
        assert_eq!(read_active_slot(&cqs).as_deref(), Some("b"));
        assert!(!slot_dir(&cqs, "a").exists());
    }

    #[test]
    fn slot_remove_refuses_last_remaining_slot() {
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = with_slots(&["only"]);
        let cqs = tmp.path().join(".cqs");
        write_active_slot(&cqs, "only").unwrap();
        let r = slot_remove(&cqs, "only", true, true);
        assert!(r.is_err());
    }

    #[test]
    fn slot_remove_non_active_works() {
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = with_slots(&["a", "b"]);
        let cqs = tmp.path().join(".cqs");
        write_active_slot(&cqs, "a").unwrap();
        slot_remove(&cqs, "b", false, true).unwrap();
        assert_eq!(read_active_slot(&cqs).as_deref(), Some("a"));
        assert!(!slot_dir(&cqs, "b").exists());
    }

    // DS-V1.30.1-D4 (#1232) — refuse to unlink a slot dir while a daemon
    // is actively serving it. These tests stand up a `UnixListener` on
    // the same socket path `daemon_status` probes and respond with a
    // `WatchSnapshot` whose `active_slot` field decides the outcome.
    //
    // The XDG-shared-state warning from `daemon_translate::tests` applies
    // here too: each test sets `XDG_RUNTIME_DIR` to a unique tempdir,
    // and the `serial_test::serial(daemon_socket_xdg)` group keeps these
    // from racing against the production-side mock-round-trip tests.

    #[cfg(unix)]
    fn make_snapshot_envelope(slot: Option<&str>) -> String {
        let snap = cqs::watch_status::WatchSnapshot {
            state: cqs::watch_status::FreshnessState::Fresh,
            modified_files: 0,
            pending_notes: false,
            rebuild_in_flight: false,
            delta_saturated: false,
            incremental_count: 0,
            dropped_this_cycle: 0,
            last_event_unix_secs: 0,
            last_synced_at: Some(0),
            snapshot_at: Some(0),
            active_slot: slot.map(|s| s.to_string()),
        };
        let inner = serde_json::json!({
            "data": serde_json::to_value(&snap).unwrap(),
            "error": null,
            "version": 1,
        });
        let outer = serde_json::json!({
            "status": "ok",
            "output": inner,
        });
        outer.to_string()
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial(daemon_socket_xdg)]
    fn slot_remove_refuses_when_daemon_serves_target() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let _g = ENV_LOCK.lock().unwrap();
        let xdg = TempDir::new().unwrap();
        let prev_xdg = std::env::var("XDG_RUNTIME_DIR").ok();
        // SAFETY: serial_test gates this; only one daemon-mock test runs at a time.
        unsafe {
            std::env::set_var("XDG_RUNTIME_DIR", xdg.path());
        }

        let tmp = with_slots(&["foo", "bar"]);
        let cqs = tmp.path().join(".cqs");
        write_active_slot(&cqs, "bar").unwrap();

        let sock_path = cqs::daemon_translate::daemon_socket_path(&cqs);
        let listener = UnixListener::bind(&sock_path).unwrap();
        let envelope = make_snapshot_envelope(Some("foo"));
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut req = String::new();
            BufReader::new(&stream).read_line(&mut req).unwrap();
            writeln!(stream, "{envelope}").unwrap();
            stream.flush().unwrap();
        });

        let result = slot_remove(&cqs, "foo", false, true);
        handle.join().unwrap();
        let _ = fs::remove_file(&sock_path);

        // SAFETY: paired with the set above.
        unsafe {
            match prev_xdg {
                Some(v) => std::env::set_var("XDG_RUNTIME_DIR", v),
                None => std::env::remove_var("XDG_RUNTIME_DIR"),
            }
        }

        let err = result.expect_err("daemon serving 'foo' must block remove");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("daemon is currently serving slot 'foo'"),
            "error should name the slot and the daemon: {msg}"
        );
        assert!(
            slot_dir(&cqs, "foo").exists(),
            "slot dir should still exist after refused remove"
        );
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial(daemon_socket_xdg)]
    fn slot_remove_with_force_proceeds_despite_daemon() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let _g = ENV_LOCK.lock().unwrap();
        let xdg = TempDir::new().unwrap();
        let prev_xdg = std::env::var("XDG_RUNTIME_DIR").ok();
        unsafe {
            std::env::set_var("XDG_RUNTIME_DIR", xdg.path());
        }

        let tmp = with_slots(&["foo", "bar"]);
        let cqs = tmp.path().join(".cqs");
        write_active_slot(&cqs, "bar").unwrap();

        let sock_path = cqs::daemon_translate::daemon_socket_path(&cqs);
        let listener = UnixListener::bind(&sock_path).unwrap();
        let envelope = make_snapshot_envelope(Some("foo"));
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut req = String::new();
            BufReader::new(&stream).read_line(&mut req).unwrap();
            writeln!(stream, "{envelope}").unwrap();
            stream.flush().unwrap();
        });

        let result = slot_remove(&cqs, "foo", true, true);
        handle.join().unwrap();
        let _ = fs::remove_file(&sock_path);

        unsafe {
            match prev_xdg {
                Some(v) => std::env::set_var("XDG_RUNTIME_DIR", v),
                None => std::env::remove_var("XDG_RUNTIME_DIR"),
            }
        }

        result.expect("--force should override daemon guard");
        assert!(
            !slot_dir(&cqs, "foo").exists(),
            "slot dir should be removed under --force"
        );
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial(daemon_socket_xdg)]
    fn slot_remove_allows_when_daemon_serves_different_slot() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let _g = ENV_LOCK.lock().unwrap();
        let xdg = TempDir::new().unwrap();
        let prev_xdg = std::env::var("XDG_RUNTIME_DIR").ok();
        unsafe {
            std::env::set_var("XDG_RUNTIME_DIR", xdg.path());
        }

        let tmp = with_slots(&["foo", "bar"]);
        let cqs = tmp.path().join(".cqs");
        write_active_slot(&cqs, "bar").unwrap();

        let sock_path = cqs::daemon_translate::daemon_socket_path(&cqs);
        let listener = UnixListener::bind(&sock_path).unwrap();
        // Daemon claims it's serving "bar"; we want to remove "foo" — should work.
        let envelope = make_snapshot_envelope(Some("bar"));
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut req = String::new();
            BufReader::new(&stream).read_line(&mut req).unwrap();
            writeln!(stream, "{envelope}").unwrap();
            stream.flush().unwrap();
        });

        let result = slot_remove(&cqs, "foo", false, true);
        handle.join().unwrap();
        let _ = fs::remove_file(&sock_path);

        unsafe {
            match prev_xdg {
                Some(v) => std::env::set_var("XDG_RUNTIME_DIR", v),
                None => std::env::remove_var("XDG_RUNTIME_DIR"),
            }
        }

        result.expect("removing a different slot must not be blocked");
        assert!(!slot_dir(&cqs, "foo").exists());
    }

    #[test]
    fn slot_active_text_path_no_panic() {
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = with_slots(&[]);
        let cqs = tmp.path().join(".cqs");
        fs::create_dir_all(&cqs).unwrap();
        // Just verify it doesn't error; output is to stdout so we can't easily
        // capture it without restructuring.
        let r = slot_active(&cqs, true);
        assert!(r.is_ok(), "{:?}", r.err());
    }

    #[test]
    fn slot_list_empty() {
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = with_slots(&[]);
        let cqs = tmp.path().join(".cqs");
        fs::create_dir_all(&cqs).unwrap();
        let r = slot_list(&cqs, true);
        assert!(r.is_ok());
    }

    /// `slots_root` is unused in this module but the import is part of the
    /// public surface — verifying it resolves keeps cqs::slot's public surface
    /// honest.
    #[test]
    fn slots_root_resolves_for_public_export() {
        let p = slots_root(Path::new("/proj/.cqs"));
        assert_eq!(p, Path::new("/proj/.cqs/slots"));
    }

    // ── P2.28: TOCTOU pin on concurrent slot lifecycle ───────────────────
    //
    // Two threads racing to create the same slot must produce a
    // deterministic outcome: exactly one Ok, the other rejected with the
    // "already exists" error. `acquire_slots_lock` (P2.61 fix) serializes
    // the read-validate-mutate window so neither thread can observe the
    // other's half-written state. Without the lock, both threads pass the
    // `dir.exists()` check, both `create_dir_all`, and one's
    // `write_slot_model` clobbers the other.
    #[test]
    fn slot_create_concurrent_same_name_produces_deterministic_outcome() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = with_slots(&[]);
        let cqs = tmp.path().join(".cqs");
        let cqs1 = cqs.clone();
        let cqs2 = cqs.clone();

        let h1 = std::thread::spawn(move || slot_create(&cqs1, "race", Some("bge-large"), true));
        let h2 = std::thread::spawn(move || slot_create(&cqs2, "race", Some("e5-base"), true));
        let r1 = h1.join().unwrap();
        let r2 = h2.join().unwrap();

        // Exactly one must succeed. With the slots.lock fix the failing
        // arm hits the `dir.exists()` bail; without the fix, both could
        // succeed and the slot.toml would be racy. Either way the OK-XOR
        // contract must hold.
        assert!(
            r1.is_ok() ^ r2.is_ok(),
            "exactly one of the racing slot_create calls must succeed (got {:?} / {:?})",
            r1.as_ref().err().map(|e| e.to_string()),
            r2.as_ref().err().map(|e| e.to_string()),
        );

        // The slot dir exists and the persisted model is one of the two
        // requested models — never an empty/half-written file.
        assert!(slot_dir(&cqs, "race").exists(), "slot dir must be present");
        let model = cqs::slot::read_slot_model(&cqs, "race");
        assert!(
            model.as_deref() == Some("bge-large") || model.as_deref() == Some("e5-base"),
            "persisted slot model must be one of the two requested values, got {:?}",
            model
        );
    }
}
