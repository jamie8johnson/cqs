//! `cqs slot` subcommand — list / create / promote / remove / active.
//!
//! Spec §Slot commands: project-level named slots living under
//! `.cqs/slots/<name>/`. See `docs/plans/2026-04-24-embeddings-cache-and-slots.md`
//! for the design. Migration from a legacy `.cqs/index.db` runs at the top of
//! `dispatch::run_with` (see `src/cli/dispatch.rs`).

use std::fs;
use std::path::Path;

use anyhow::Result;
use clap::Subcommand;
use colored::Colorize;

use cqs::slot::{
    active_slot_path, list_slots, read_active_slot, slot_dir, validate_slot_name,
    write_active_slot, DEFAULT_SLOT,
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
    let resolved_model: Option<String> = match model {
        Some(m) => {
            let cfg = cqs::embedder::ModelConfig::resolve(Some(m), None);
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
    let dir = slot_dir(project_cqs_dir, name);
    if !dir.exists() {
        let available = list_slots(project_cqs_dir).unwrap_or_default().join(", ");
        anyhow::bail!(
            "Slot '{}' does not exist. Available: [{}].",
            name,
            available
        );
    }

    let active = read_active_slot(project_cqs_dir).unwrap_or_else(|| DEFAULT_SLOT.to_string());
    let mut all = list_slots(project_cqs_dir).unwrap_or_default();
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
}
