//! Project-level named slots — side-by-side full indexes under `.cqs/slots/<name>/`.
//!
//! A slot is a self-contained index: its own SQLite `index.db`, HNSW files, and
//! SPLADE artifacts. Slots let users keep multiple embedders side-by-side with
//! atomic `cqs slot promote` switching, instead of destructive
//! `cqs model swap` reindex cycles.
//!
//! # Layout
//!
//! ```text
//! .cqs/
//!   active_slot                # text file: bare slot name, e.g. "default"
//!   embeddings_cache.db        # cross-slot, content-addressed
//!   slots/
//!     default/                 # post-migration of legacy `.cqs/index.db`
//!       index.db
//!       hnsw_*.bin
//!       splade.index.bin
//!     e5/                      # user-created via `cqs slot create`
//!       index.db
//!       …
//!   watch.sock                 # daemon socket — bound to whichever slot was
//!                              # active at daemon startup
//! ```
//!
//! # Resolution order
//!
//! `resolve_slot_name` consults, in priority order:
//!
//! 1. `--slot <name>` flag (caller-supplied)
//! 2. `CQS_SLOT` env var
//! 3. `.cqs/active_slot` file content (trimmed, validated)
//! 4. Hardcoded fallback `"default"`
//!
//! Each step logs at `info` (or `debug` for the file/fallback) so troubleshooting
//! a "wrong slot" report is one `RUST_LOG=cqs=debug` away.
//!
//! # Migration
//!
//! [`migrate_legacy_index_to_default_slot`] runs idempotently on every
//! `Store::open`. If `.cqs/index.db` exists AND `.cqs/slots/` does not, it
//! moves `index.db` + HNSW + SPLADE into `.cqs/slots/default/` and writes
//! `.cqs/active_slot = "default"`. Failures roll back via inventory.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Maximum bytes read from any slot pointer file. Slot config and active-slot
/// pointer files are tiny (slot.toml is ~50 bytes; active_slot is ~10), so
/// 4 KiB is two orders of magnitude headroom. Without this cap, an attacker or
/// FS bug producing a multi-GB pointer would OOM every CLI invocation that
/// touches slots (every `cqs index`, every `cqs search`, etc.).
const SLOT_POINTER_MAX_BYTES: u64 = 4096;

use thiserror::Error;

/// Bare directory name under `.cqs/` that holds per-slot dirs.
pub const SLOTS_DIR: &str = "slots";

/// Bare file name of the active-slot pointer in `.cqs/`.
pub const ACTIVE_SLOT_FILE: &str = "active_slot";

/// Bare file name of the per-slot config in `.cqs/slots/<name>/`.
pub const SLOT_CONFIG_FILE: &str = "slot.toml";

/// Default slot name, used when nothing else resolves.
pub const DEFAULT_SLOT: &str = "default";

/// Maximum slot name length (matches spec §Slot commands).
pub const MAX_SLOT_NAME_LEN: usize = 32;

/// Reserved slot names — names used as subcommand verbs, plus `default`
/// (pre-claimed for the migration target). Rejecting these at create time
/// keeps `cqs slot create active` and friends from producing surprising
/// `cqs slot active` collisions.
const RESERVED_SLOT_NAMES: &[&str] = &[
    "active", "list", "create", "promote", "remove", "stats", "prune", "compact",
];

/// Errors that can occur during slot resolution / lifecycle / migration.
#[derive(Debug, Error)]
pub enum SlotError {
    #[error("Slot name is empty (must match [a-z0-9_-]+, max {MAX_SLOT_NAME_LEN} chars)")]
    EmptyName,

    #[error(
        "Slot name '{name}' is too long (max {max} chars; got {got})",
        max = MAX_SLOT_NAME_LEN,
        got = name.chars().count()
    )]
    NameTooLong { name: String },

    #[error("Slot name '{0}' contains invalid character(s) (allowed: a-z, 0-9, _, -)")]
    InvalidCharacters(String),

    #[error("Slot name '{0}' is reserved (collides with a subcommand verb or pre-claimed name)")]
    Reserved(String),

    #[error("Slot '{0}' does not exist. Available: [{1}]. Create with: cqs slot create <name> --model <model-id>")]
    NotFound(String, String),

    #[error("Slot '{0}' exists but has no index.db. Run `cqs index --slot {0}` first.")]
    Empty(String),

    #[error("Cannot remove the active slot '{0}' without --force, or while it is the only slot. Promote another slot first.")]
    RemoveActive(String),

    #[error("At least one slot must remain. Refusing to remove the last slot '{0}'.")]
    RemoveLast(String),

    #[error("Filesystem error while operating on slot '{slot}': {source}")]
    Io {
        slot: String,
        #[source]
        source: std::io::Error,
    },

    #[error("Migration failed: {0}")]
    Migration(String),
}

/// Where a slot name came from. Surfaced in `tracing::info!` so a wrong-slot
/// report can be diagnosed in one log search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotSource {
    /// Caller supplied an explicit `--slot <name>` flag.
    Flag,
    /// `CQS_SLOT` environment variable.
    Env,
    /// `.cqs/active_slot` pointer file.
    File,
    /// Hardcoded `"default"` fallback (no other source resolved).
    Fallback,
}

impl SlotSource {
    /// String form for tracing fields.
    pub fn as_str(self) -> &'static str {
        match self {
            SlotSource::Flag => "flag",
            SlotSource::Env => "env",
            SlotSource::File => "file",
            SlotSource::Fallback => "fallback",
        }
    }
}

/// A resolved slot name + the source that produced it.
#[derive(Debug, Clone)]
pub struct ResolvedSlot {
    /// Validated slot name.
    pub name: String,
    /// Source the name came from (flag/env/file/fallback).
    pub source: SlotSource,
}

/// Validate a slot name per spec §Slot commands: `[a-z0-9_-]+`, max 32 chars,
/// reserved names rejected.
///
/// Reserved names are subcommand verbs (`active`, `list`, `create`, etc.) plus
/// `default` is permitted (used by migration). The migration creates `default`
/// programmatically — explicit `cqs slot create default` is also allowed; the
/// pre-claim is in [`RESERVED_SLOT_NAMES`] and `default` is intentionally
/// absent from that list.
pub fn validate_slot_name(name: &str) -> Result<(), SlotError> {
    if name.is_empty() {
        return Err(SlotError::EmptyName);
    }
    if name.chars().count() > MAX_SLOT_NAME_LEN {
        return Err(SlotError::NameTooLong {
            name: name.to_string(),
        });
    }
    let valid_chars = name
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-');
    if !valid_chars {
        return Err(SlotError::InvalidCharacters(name.to_string()));
    }
    if name.starts_with('-') || name.ends_with('-') {
        // Leading dash collides with clap's flag parser ("cqs slot promote -foo"
        // gets interpreted as an unknown flag); trailing dashes are stripped by
        // common copy-paste/shell pipelines and produce subtle name drift.
        return Err(SlotError::InvalidCharacters(name.to_string()));
    }
    if RESERVED_SLOT_NAMES.contains(&name) {
        return Err(SlotError::Reserved(name.to_string()));
    }
    Ok(())
}

/// Path of `.cqs/slots/<name>/` for the given project `.cqs/` dir + slot name.
///
/// SEC-V1.36-3: validates the slot name even on read paths. Write paths
/// already call [`validate_slot_name`] up front, but read paths
/// (`read_slot_model`, the public `cqs::resolve_slot_dir`, etc.) used to
/// trust the caller. A `..`-bearing name would have produced a path that
/// `Path::join` does *not* normalize, so callers passing attacker-controlled
/// strings without their own validation could escape the slots dir. On
/// failure we substitute a sentinel name so downstream IO fails noisily
/// inside the slots directory rather than silently traversing outside it.
pub fn slot_dir(project_cqs_dir: &Path, slot_name: &str) -> PathBuf {
    if validate_slot_name(slot_name).is_err() {
        tracing::warn!(
            slot = %slot_name,
            "slot_dir called with invalid slot name; substituting `__invalid__` to keep IO inside slots dir"
        );
        return project_cqs_dir.join(SLOTS_DIR).join("__invalid__");
    }
    project_cqs_dir.join(SLOTS_DIR).join(slot_name)
}

/// Bare file name of the slot lifecycle lockfile under `.cqs/`.
pub const SLOTS_LOCK_FILE: &str = "slots.lock";

/// Acquire an exclusive `flock` on `.cqs/slots.lock`. Held for the duration of
/// any slot lifecycle operation (create / promote / remove) so concurrent
/// invocations across processes serialize their read-validate-mutate
/// sequences. The lock file is created if missing.
///
/// Defends against P2.61 / P2.28 (TOCTOU on concurrent promote+remove that
/// would leave `active_slot` pointing at a deleted directory). Callers should
/// hold the returned `File` for the duration of the slot mutation; dropping
/// the file releases the OS lock.
pub fn acquire_slots_lock(project_cqs_dir: &Path) -> Result<fs::File, SlotError> {
    if !project_cqs_dir.exists() {
        fs::create_dir_all(project_cqs_dir).map_err(|source| SlotError::Io {
            slot: SLOTS_LOCK_FILE.to_string(),
            source,
        })?;
    }
    let path = project_cqs_dir.join(SLOTS_LOCK_FILE);
    let f = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(&path)
        .map_err(|source| SlotError::Io {
            slot: SLOTS_LOCK_FILE.to_string(),
            source,
        })?;
    // std::fs::File::lock (Rust 1.89+) blocks until exclusive ownership is
    // acquired across processes. MSRV 1.95 covers it.
    f.lock().map_err(|source| SlotError::Io {
        slot: SLOTS_LOCK_FILE.to_string(),
        source,
    })?;
    Ok(f)
}

/// Path of `.cqs/slots/` for the given project `.cqs/` dir.
pub fn slots_root(project_cqs_dir: &Path) -> PathBuf {
    project_cqs_dir.join(SLOTS_DIR)
}

/// Path of `.cqs/slots/<name>/slot.toml` — the per-slot config file.
pub fn slot_config_path(project_cqs_dir: &Path, slot_name: &str) -> PathBuf {
    slot_dir(project_cqs_dir, slot_name).join(SLOT_CONFIG_FILE)
}

/// Read the embedding model preset/repo persisted in `.cqs/slots/<name>/slot.toml`.
///
/// Schema (#1107):
/// ```toml
/// [embedding]
/// model = "nomic-coderank"
/// ```
///
/// Returns `None` if the file is missing, unreadable, or has no `[embedding].model`.
/// Caller falls back to the next priority in `ModelConfig::resolve`.
pub fn read_slot_model(project_cqs_dir: &Path, slot_name: &str) -> Option<String> {
    let path = slot_config_path(project_cqs_dir, slot_name);
    // P2.33: bound the read so a pathological slot.toml (multi-GB or
    // unbounded growth) can't OOM every CLI invocation. 4 KiB is ~80x the
    // realistic slot.toml size.
    let raw = match fs::File::open(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "Failed to open slot config; falling back to default resolution"
            );
            return None;
        }
        Ok(f) => {
            let mut buf = String::new();
            if let Err(e) = f.take(SLOT_POINTER_MAX_BYTES).read_to_string(&mut buf) {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "Bounded read of slot config failed; falling back to default resolution"
                );
                return None;
            }
            buf
        }
    };
    match toml::from_str::<SlotConfigFile>(&raw) {
        Ok(cfg) => cfg.embedding.and_then(|e| e.model),
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "Slot config is malformed TOML; falling back to default resolution"
            );
            None
        }
    }
}

/// Write `model` into `.cqs/slots/<name>/slot.toml [embedding].model`.
///
/// Read-modify-write: parses the existing slot.toml (if any) into a
/// structured `SlotConfigFile`, sets `[embedding].model`, serializes
/// the whole struct back through `toml::to_string`, and atomically
/// replaces the file. Any additional top-level sections present on disk
/// (e.g. a future `[reranker]` or a hand-added `[notes]`) are preserved
/// verbatim via the `#[serde(flatten)] extra: toml::Table` catch-all on
/// `SlotConfigFile` — adding a new typed section to the struct in the
/// future requires zero changes to this function (#1217). Comments are
/// not preserved; the toml crate does not retain them across a
/// deserialize/serialize round-trip.
///
/// Atomic via temp+rename + parent-dir fsync (`crate::fs::atomic_replace`).
/// Creates the slot dir if missing (idempotent).
///
/// P3.39: routes through `crate::fs::atomic_replace` so the parent directory
/// is fsynced after the rename — matches the durability contract of
/// `notes.toml` / `audit-mode.json`.
pub fn write_slot_model(
    project_cqs_dir: &Path,
    slot_name: &str,
    model: &str,
) -> Result<(), SlotError> {
    validate_slot_name(slot_name)?;
    let dir = slot_dir(project_cqs_dir, slot_name);
    if !dir.exists() {
        fs::create_dir_all(&dir).map_err(|source| SlotError::Io {
            slot: slot_name.to_string(),
            source,
        })?;
    }
    let final_path = slot_config_path(project_cqs_dir, slot_name);

    // Read existing config (if any) into a typed struct, mutate the
    // model field, write the full struct back. Unknown sections survive
    // via the flatten catch-all on `SlotConfigFile`. A malformed TOML on
    // disk is treated as "default" rather than a hard error so a
    // corrupted slot.toml still recovers on the next write — matches the
    // tolerance pattern in `read_slot_model` (warn + None) and means
    // `cqs slot promote` can't deadlock on a hand-broken config.
    // RB-V1.36-4: cap slot.toml read at the small-file budget. The sibling
    // `Config::load_file` path enforces MAX_CONFIG_SIZE; this site was the
    // only one without a guard.
    let mut config: SlotConfigFile = if final_path.exists() {
        let max_bytes = crate::limits::small_file_max_bytes();
        let oversize = fs::metadata(&final_path)
            .map(|m| m.len() > max_bytes)
            .unwrap_or(false);
        if oversize {
            tracing::warn!(
                path = %final_path.display(),
                cap = max_bytes,
                "slot.toml exceeds CQS_SMALL_FILE_MAX_BYTES; rewriting from default"
            );
            SlotConfigFile::default()
        } else {
            match fs::read_to_string(&final_path) {
                Ok(raw) => toml::from_str(&raw).unwrap_or_else(|e| {
                    tracing::warn!(
                        path = %final_path.display(),
                        error = %e,
                        "Existing slot.toml is malformed; rewriting from default"
                    );
                    SlotConfigFile::default()
                }),
                Err(e) => {
                    tracing::warn!(
                        path = %final_path.display(),
                        error = %e,
                        "Failed to read slot.toml for round-trip; rewriting from default"
                    );
                    SlotConfigFile::default()
                }
            }
        }
    } else {
        SlotConfigFile::default()
    };
    let embedding = config
        .embedding
        .get_or_insert_with(SlotEmbeddingSection::default);
    embedding.model = Some(model.to_string());

    let body = toml::to_string(&config).map_err(|e| SlotError::Io {
        slot: slot_name.to_string(),
        source: std::io::Error::new(std::io::ErrorKind::InvalidData, e),
    })?;

    // DS-V1.33-2: include `crate::temp_suffix()` so concurrent writers (e.g.
    // legacy migration in one CLI process while another `cqs slot promote`
    // runs) each stage to their own temp file before atomic_replace, instead
    // of racing on a fixed `slot.toml.tmp` path.
    let suffix = crate::temp_suffix();
    let tmp_path = dir.join(format!("{}.{:016x}.tmp", SLOT_CONFIG_FILE, suffix));
    {
        let mut f = fs::File::create(&tmp_path).map_err(|source| SlotError::Io {
            slot: slot_name.to_string(),
            source,
        })?;
        f.write_all(body.as_bytes())
            .map_err(|source| SlotError::Io {
                slot: slot_name.to_string(),
                source,
            })?;
        // atomic_replace re-opens the temp and calls sync_all itself, so a
        // second sync_all here would be redundant.
    }
    crate::fs::atomic_replace(&tmp_path, &final_path).map_err(|source| {
        // Best-effort cleanup: atomic_replace cleans up its own cross-device
        // temp on failure, but the source temp may remain on a same-device
        // rename failure path.
        let _ = fs::remove_file(&tmp_path);
        SlotError::Io {
            slot: slot_name.to_string(),
            source,
        }
    })?;
    Ok(())
}

/// Serde shape for `slot.toml`.
///
/// `extra` captures unknown top-level keys via `#[serde(flatten)]` so a
/// hand-added `[notes].project_id = "foo"` (or a future typed section
/// not yet promoted to a struct field) survives a round-trip through
/// `write_slot_model`. When a new typed section lands (e.g. `reranker:
/// Option<SlotRerankerSection>`), it can be lifted from `extra` into a
/// dedicated field with no migration — TOML parsing puts new fields
/// into named slots first, leftovers into `extra`.
#[derive(Default, serde::Deserialize, serde::Serialize)]
struct SlotConfigFile {
    #[serde(skip_serializing_if = "Option::is_none")]
    embedding: Option<SlotEmbeddingSection>,
    #[serde(default, flatten, skip_serializing_if = "toml::Table::is_empty")]
    extra: toml::Table,
}

#[derive(Default, serde::Deserialize, serde::Serialize)]
struct SlotEmbeddingSection {
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
}

/// Path of `.cqs/active_slot` pointer file.
pub fn active_slot_path(project_cqs_dir: &Path) -> PathBuf {
    project_cqs_dir.join(ACTIVE_SLOT_FILE)
}

/// Read the active slot name from `.cqs/active_slot`. Returns `None` if the
/// file is missing or empty / corrupt — caller falls back to `DEFAULT_SLOT`.
///
/// Corruption (non-UTF8, invalid characters) is logged at `warn` and treated
/// as missing so a single mangled write doesn't render the project unusable.
pub fn read_active_slot(project_cqs_dir: &Path) -> Option<String> {
    let path = active_slot_path(project_cqs_dir);
    // P2.33: bound the read of the active-slot pointer so an oversize file
    // can't OOM every CLI invocation. 4 KiB is two orders of magnitude
    // headroom on the ~10 byte realistic content (`default\n`, etc.).
    let raw = match fs::File::open(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "Failed to open active_slot pointer; falling back to default"
            );
            return None;
        }
        Ok(f) => {
            let mut buf = String::new();
            if let Err(e) = f.take(SLOT_POINTER_MAX_BYTES).read_to_string(&mut buf) {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "Bounded read of active_slot pointer failed; falling back to default"
                );
                return None;
            }
            buf
        }
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        tracing::warn!(
            path = %path.display(),
            "active_slot file is empty; falling back to default"
        );
        return None;
    }
    match validate_slot_name(trimmed) {
        Ok(()) => Some(trimmed.to_string()),
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                contents = %trimmed,
                error = %e,
                "active_slot file contains invalid slot name; falling back to default"
            );
            None
        }
    }
}

/// Write the active slot pointer atomically. Validates the name first.
///
/// Writes to a sibling `<active_slot>.tmp` then routes through
/// `crate::fs::atomic_replace` for the rename + parent-dir fsync. Atomic on
/// the same filesystem; crash between write and rename leaves the previous
/// pointer intact.
///
/// P3.39: routes through `crate::fs::atomic_replace` so the parent directory
/// is fsynced after the rename — matches the durability contract of
/// `notes.toml` / `audit-mode.json`.
pub fn write_active_slot(project_cqs_dir: &Path, slot_name: &str) -> Result<(), SlotError> {
    validate_slot_name(slot_name)?;
    let _span = tracing::info_span!(
        "write_active_slot",
        slot_name,
        cqs_dir = %project_cqs_dir.display()
    )
    .entered();

    if !project_cqs_dir.exists() {
        fs::create_dir_all(project_cqs_dir).map_err(|source| SlotError::Io {
            slot: slot_name.to_string(),
            source,
        })?;
    }

    let final_path = active_slot_path(project_cqs_dir);
    // DS-V1.33-2: include `crate::temp_suffix()` so concurrent writers (e.g.
    // legacy migration in one CLI process while another `cqs slot promote`
    // runs) each stage to their own temp file before atomic_replace, instead
    // of racing on a fixed `active_slot.tmp` path.
    let suffix = crate::temp_suffix();
    let tmp_path = project_cqs_dir.join(format!("{}.{:016x}.tmp", ACTIVE_SLOT_FILE, suffix));

    {
        let mut f = fs::File::create(&tmp_path).map_err(|source| SlotError::Io {
            slot: slot_name.to_string(),
            source,
        })?;
        f.write_all(slot_name.as_bytes())
            .map_err(|source| SlotError::Io {
                slot: slot_name.to_string(),
                source,
            })?;
        // atomic_replace re-opens the temp and calls sync_all itself — no
        // explicit fsync needed here.
    }

    crate::fs::atomic_replace(&tmp_path, &final_path).map_err(|source| {
        let _ = fs::remove_file(&tmp_path);
        SlotError::Io {
            slot: slot_name.to_string(),
            source,
        }
    })?;
    tracing::info!(slot_name, "active slot pointer updated");
    Ok(())
}

/// List all slot directories under `.cqs/slots/`. Each entry is the bare slot
/// name (the directory's file name). Returns an empty Vec when `slots/`
/// doesn't exist yet.
///
/// Sorted alphabetically so output is deterministic for tests + UI.
pub fn list_slots(project_cqs_dir: &Path) -> Result<Vec<String>, SlotError> {
    let _span = tracing::debug_span!("list_slots", cqs_dir = %project_cqs_dir.display()).entered();
    let root = slots_root(project_cqs_dir);
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut names = Vec::new();
    let entries = fs::read_dir(&root).map_err(|source| SlotError::Io {
        slot: String::new(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| SlotError::Io {
            slot: String::new(),
            source,
        })?;
        if !entry
            .file_type()
            .map_err(|source| SlotError::Io {
                slot: String::new(),
                source,
            })?
            .is_dir()
        {
            continue;
        }
        if let Some(name) = entry.file_name().to_str() {
            // Skip dirs that don't validate as slot names (e.g., temp dirs).
            if validate_slot_name(name).is_ok() {
                names.push(name.to_string());
            } else {
                tracing::debug!(name, "Skipping non-slot directory under slots/");
            }
        }
    }
    names.sort();
    Ok(names)
}

/// Resolve the slot name from the documented source order:
/// `--slot` flag > `CQS_SLOT` env > `.cqs/active_slot` file > `"default"`.
///
/// Returns the resolved name plus the source for tracing.
pub fn resolve_slot_name(
    flag: Option<&str>,
    project_cqs_dir: &Path,
) -> Result<ResolvedSlot, SlotError> {
    let _span = tracing::debug_span!("resolve_slot_name").entered();

    if let Some(name) = flag {
        validate_slot_name(name)?;
        tracing::info!(slot = name, source = "flag", "active slot resolved");
        return Ok(ResolvedSlot {
            name: name.to_string(),
            source: SlotSource::Flag,
        });
    }
    if let Ok(env_name) = std::env::var("CQS_SLOT") {
        let env_name = env_name.trim();
        if !env_name.is_empty() {
            validate_slot_name(env_name)?;
            tracing::info!(slot = env_name, source = "env", "active slot resolved");
            return Ok(ResolvedSlot {
                name: env_name.to_string(),
                source: SlotSource::Env,
            });
        }
    }
    if let Some(name) = read_active_slot(project_cqs_dir) {
        tracing::debug!(slot = %name, source = "file", "active slot resolved");
        return Ok(ResolvedSlot {
            name,
            source: SlotSource::File,
        });
    }
    tracing::debug!(
        slot = DEFAULT_SLOT,
        source = "fallback",
        "active slot resolved"
    );
    Ok(ResolvedSlot {
        name: DEFAULT_SLOT.to_string(),
        source: SlotSource::Fallback,
    })
}

/// Bare file name of the migration sentinel — present while the legacy →
/// slots/default migration is in progress, removed only on full success.
/// Subsequent migration calls refuse to proceed if the sentinel exists, so
/// half-state from a crashed migration becomes a loud signal instead of an
/// undetectable split between `.cqs/` and `.cqs/slots/default/`.
pub const MIGRATION_SENTINEL_FILE: &str = "migration.lock";

/// One-shot filesystem migration: move legacy `.cqs/index.db` (and its HNSW /
/// SPLADE sidecars) into `.cqs/slots/default/`, then write
/// `.cqs/active_slot = "default"`.
///
/// Idempotent: if `.cqs/slots/` already exists, this is a no-op. Atomic where
/// the source and destination live on the same filesystem (common case);
/// otherwise falls back to copy + delete with an inventory-based rollback on
/// partial failure.
///
/// # Half-state robustness (P2.34)
///
/// A `.cqs/migration.lock` sentinel is written before any file moves and only
/// removed on full success. If a previous call crashed mid-migration the
/// sentinel persists; the next call refuses to proceed and surfaces the
/// previous failure context for manual recovery. This converts an undetectable
/// split (`.cqs/index.db` AND `.cqs/slots/default/index.db` both present, or
/// neither) into a loud, recoverable signal.
///
/// # WAL drain (P2.62)
///
/// Before any file moves, we open the legacy DB and run
/// `PRAGMA wal_checkpoint(TRUNCATE)` so uncommitted WAL pages are flushed into
/// the main DB. Without this, a non-atomic cross-device move (the EXDEV
/// fallback in `move_file`) could land `index.db` and `index.db-wal` on the
/// destination at different times; a crash between the two leaves the new
/// slot's DB without its WAL and SQLite silently truncates uncommitted pages.
///
/// Returns:
/// - `Ok(true)` if migration ran (legacy → slots/default/)
/// - `Ok(false)` if no legacy state was found, or `slots/` already exists
pub fn migrate_legacy_index_to_default_slot(project_cqs_dir: &Path) -> Result<bool, SlotError> {
    let _span = tracing::info_span!(
        "migrate_legacy_index_to_default_slot",
        cqs_dir = %project_cqs_dir.display()
    )
    .entered();

    if !project_cqs_dir.exists() {
        return Ok(false);
    }

    // DS-V1.33-1: serialize with other slot lifecycle operations. Two
    // concurrent CLI invocations on a fresh-clone project (e.g. a watch
    // daemon starting at the same moment as `cqs search`) both observed the
    // pre-migration state, both passed the sentinel check, and both kicked
    // off the move loop — leaving sidecars split across `.cqs/` and
    // `slots/default/` because the rollback only ran in the loser. Acquiring
    // the same `slots.lock` that `slot_create/promote/remove` hold makes
    // these checks-and-mutates atomic per `.cqs/` directory. Held until the
    // function returns (the `_lock` binding is dropped on every exit path).
    let _lock = acquire_slots_lock(project_cqs_dir)?;

    let slots_dir = slots_root(project_cqs_dir);
    if slots_dir.exists() {
        return Ok(false);
    }

    let legacy_index = project_cqs_dir.join(crate::INDEX_DB_FILENAME);
    if !legacy_index.exists() {
        // Nothing to migrate; create `slots/` so subsequent runs treat the
        // project as slot-aware. Without this a fresh project never enters
        // slot-aware mode until first index.
        return Ok(false);
    }

    // P2.34: refuse to proceed if a previous migration crashed mid-flight.
    // Sentinel content tells the operator exactly what went wrong and which
    // files (if any) had already moved — see the failure-arm `fs::write` below.
    let sentinel = project_cqs_dir.join(MIGRATION_SENTINEL_FILE);
    if sentinel.exists() {
        // RB-5: cap the sentinel read at 64 KiB. The sentinel is written
        // by the failure-arm `fs::write` below as a tiny key=value blurb;
        // anything larger means corruption (or a hostile tree). Reading
        // GiB-scale "sentinel" files into memory would OOM the process.
        const SENTINEL_MAX_BYTES: u64 = 64 * 1024;
        // EH-V1.36-7 / P3: distinguish "sentinel exists but unreadable" from
        // "sentinel exists and was empty" so the operator can tell whether
        // they need to chmod / fix perms before deleting the file.
        let detail = {
            let mut buf = String::new();
            match fs::File::open(&sentinel).and_then(|f| {
                f.take(SENTINEL_MAX_BYTES)
                    .read_to_string(&mut buf)
                    .map(|_| ())
            }) {
                Ok(_) => buf,
                Err(e) => format!("(could not read sentinel: {})", e),
            }
        };
        return Err(SlotError::Migration(format!(
            "previous migration failed (see {}). Manually recover then `rm {}`. \
             Sentinel contents:\n{}",
            sentinel.display(),
            sentinel.display(),
            detail
        )));
    }

    // P2.34 / DS-V1.33-9: write the sentinel FIRST — before we touch the FS
    // for any reason, including the WAL checkpoint below. The checkpoint is a
    // FS mutation (it truncates the WAL and removes uncommitted page state
    // from the live DB), so a crash between checkpoint and sentinel-write
    // leaves "WAL drained, no breadcrumb" — the next migration call cannot
    // tell that anything happened and proceeds as a fresh migration.
    let started_at = chrono::Utc::now().to_rfc3339();
    if let Err(e) = fs::write(
        &sentinel,
        format!("started_at={}\nstate=in_progress\n", started_at),
    ) {
        tracing::warn!(
            error = %e,
            path = %sentinel.display(),
            "Failed to write migration sentinel; migration will proceed without crash protection"
        );
    }

    // P2.62: drain WAL before moving files. Failure is non-fatal — same-fs
    // renames are atomic per file so the WAL/SHM either move with index.db or
    // not at all. Cross-device moves (EXDEV fallback) accept the residual risk
    // and we log loudly so operators can correlate any post-migration loss.
    if let Err(e) = checkpoint_legacy_index(&legacy_index) {
        tracing::warn!(
            error = %e,
            db = %legacy_index.display(),
            "Failed to checkpoint legacy index.db before migration; cross-device \
             move may lose uncommitted WAL pages"
        );
    }

    let dest = slot_dir(project_cqs_dir, DEFAULT_SLOT);

    fs::create_dir_all(&dest).map_err(|source| SlotError::Io {
        slot: DEFAULT_SLOT.to_string(),
        source,
    })?;

    // Inventory of files we plan to move. Order matters: index.db first so
    // failures after that point are recoverable (we leave `slots/default/`
    // populated with whatever moved, plus the legacy file restored).
    let migration_files = collect_migration_files(project_cqs_dir);
    let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();

    for src in &migration_files {
        let file_name = match src.file_name() {
            Some(n) => n.to_string_lossy().into_owned(),
            None => continue,
        };
        let dst = dest.join(&file_name);
        if let Err(e) = move_file(src, &dst) {
            // Rollback — restore everything we already moved.
            tracing::error!(
                src = %src.display(),
                dst = %dst.display(),
                error = %e,
                "migration step failed; rolling back"
            );
            let mut rollback_failures: Vec<String> = Vec::new();
            for (already_dst, already_src) in moved.iter().rev() {
                if let Err(rollback_err) = move_file(already_dst, already_src) {
                    tracing::error!(
                        src = %already_dst.display(),
                        dst = %already_src.display(),
                        error = %rollback_err,
                        "rollback failed (manual recovery may be needed)"
                    );
                    rollback_failures.push(format!(
                        "{} -> {}: {}",
                        already_dst.display(),
                        already_src.display(),
                        rollback_err
                    ));
                }
            }
            // Best-effort: clean up the empty slots/default/ + slots/ if rollback was clean.
            let _ = fs::remove_dir(&dest);
            let _ = fs::remove_dir(&slots_dir);

            // P2.34: persist failure context so the next migration call (which
            // will refuse to proceed) tells the operator exactly what happened.
            // Sentinel stays in place — operator must `rm` it after manual
            // recovery, which doubles as the "I have looked at this" gate.
            let _ = fs::write(
                &sentinel,
                format!(
                    "started_at={}\nfailed_at={}\nstate=failed\nfailed_step={} -> {}\nreason={}\nrollback_failures={:?}\n",
                    started_at,
                    chrono::Utc::now().to_rfc3339(),
                    src.display(),
                    dst.display(),
                    e,
                    rollback_failures
                ),
            );
            return Err(SlotError::Migration(format!(
                "failed to move {}: {}",
                src.display(),
                e
            )));
        }
        moved.push((dst, src.clone()));
    }

    // Finalize by writing the active_slot pointer.
    write_active_slot(project_cqs_dir, DEFAULT_SLOT)?;

    // P2.34: success — remove the sentinel as the last step. If the process
    // dies after the moves but before this remove, the next call will refuse
    // to migrate but the slot is already in place; operator can simply
    // delete the sentinel.
    if let Err(e) = fs::remove_file(&sentinel) {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(
                error = %e,
                path = %sentinel.display(),
                "Failed to remove migration sentinel after successful migration; \
                 next call will refuse to migrate until removed manually"
            );
        }
    }

    tracing::info!(
        files_moved = moved.len(),
        from = %project_cqs_dir.display(),
        to = %dest.display(),
        "legacy index.db migrated to slots/default/"
    );
    Ok(true)
}

/// Open the legacy DB and run `PRAGMA wal_checkpoint(TRUNCATE)` so the WAL
/// sidecar is empty before the migration moves files. Closes the connection
/// before returning so file handles don't leak into the move loop.
///
/// Used by [`migrate_legacy_index_to_default_slot`] to defend against
/// non-atomic cross-device moves losing uncommitted WAL pages (P2.62).
fn checkpoint_legacy_index(legacy_index: &Path) -> Result<(), SlotError> {
    use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode};
    use sqlx::{ConnectOptions, Connection};
    use std::str::FromStr;

    // Build a single-thread runtime for the pragma — slot migration is the
    // only caller and runs at most once per project lifetime.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| SlotError::Migration(format!("checkpoint runtime: {e}")))?;

    rt.block_on(async {
        let opts = SqliteConnectOptions::from_str(&format!("sqlite://{}", legacy_index.display()))
            .map_err(|e| SlotError::Migration(format!("checkpoint open: {e}")))?
            .journal_mode(SqliteJournalMode::Wal)
            .create_if_missing(false);
        let mut conn = opts
            .connect()
            .await
            .map_err(|e| SlotError::Migration(format!("checkpoint connect: {e}")))?;
        sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
            .execute(&mut conn)
            .await
            .map_err(|e| SlotError::Migration(format!("checkpoint pragma: {e}")))?;
        // Drop the connection explicitly so file handles are released before
        // the caller's move loop starts.
        let _ = conn.close().await;
        Ok::<(), SlotError>(())
    })
}

/// Collect every file we want to migrate from `.cqs/` → `.cqs/slots/default/`.
/// Hardcoded list of patterns since there's no manifest of "slot-local" files
/// in the rest of the codebase.
fn collect_migration_files(project_cqs_dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    // Always-present
    // DS-V1.36-1: include the full HNSW sidecar set. The previous list
    // missed `hnsw.ids` and `hnsw.checksum` for both basenames. Post-PR #1325,
    // verify_hnsw_checksums treats a checksum sidecar referencing files that
    // don't exist as a hard error — a legacy-slot migration would leave
    // .ids/.checksum behind in `.cqs/` and the next search either failed to
    // verify or rebuilt from scratch (losing all enrichment work).
    let candidates = [
        crate::INDEX_DB_FILENAME,
        "index.db-wal",
        "index.db-shm",
        "index.db.bak",
        // HNSW (enriched + base, full sidecar set)
        "index.hnsw.data",
        "index.hnsw.graph",
        "index.hnsw.ids",
        "index.hnsw.checksum",
        "index_base.hnsw.data",
        "index_base.hnsw.graph",
        "index_base.hnsw.ids",
        "index_base.hnsw.checksum",
        "index.hnsw.lock",
        "index_base.hnsw.lock",
        "index.cagra",
        "index.cagra.sidecar",
        "index.cagra.meta",
        // SPLADE
        "splade.index.bin",
        "splade.index.bin.bak",
    ];
    for name in candidates {
        let p = project_cqs_dir.join(name);
        if p.exists() {
            out.push(p);
        }
    }
    out
}

/// Move a file, atomic where possible; falls back to copy + remove on **any**
/// rename failure.
///
/// Previously this matched on a hardcoded `EXDEV` errno (18 on Linux/macOS,
/// `ERROR_NOT_SAME_DEVICE = 17` on Windows) which silently mis-classified the
/// cross-device case on Windows. Falling back unconditionally is cheaper than
/// tracking platform-specific errno constants — if the source is gone or the
/// destination unwritable, the caller surfaces the I/O error from `copy()`
/// instead.
fn move_file(src: &Path, dst: &Path) -> std::io::Result<()> {
    match fs::rename(src, dst) {
        Ok(()) => Ok(()),
        Err(_) => {
            fs::copy(src, dst)?;
            fs::remove_file(src)?;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Mutex;
    use tempfile::TempDir;

    /// Env-touching tests must serialize: `std::env::set_var` is process-global.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    // ── name validation ──────────────────────────────────────────────────

    #[test]
    fn validate_accepts_lowercase_alphanumeric() {
        assert!(validate_slot_name("default").is_ok());
        assert!(validate_slot_name("e5").is_ok());
        assert!(validate_slot_name("bge_large").is_ok());
        assert!(validate_slot_name("v9-200k").is_ok());
        assert!(validate_slot_name("a1b2c3").is_ok());
        assert!(validate_slot_name("a").is_ok());
    }

    #[test]
    fn validate_rejects_uppercase() {
        assert!(matches!(
            validate_slot_name("E5"),
            Err(SlotError::InvalidCharacters(_))
        ));
        assert!(matches!(
            validate_slot_name("Default"),
            Err(SlotError::InvalidCharacters(_))
        ));
    }

    #[test]
    fn validate_rejects_spaces_and_punct() {
        for bad in ["my slot", "my.slot", "my/slot", "my!slot", "slot."] {
            assert!(
                matches!(
                    validate_slot_name(bad),
                    Err(SlotError::InvalidCharacters(_))
                ),
                "should reject {bad}"
            );
        }
    }

    #[test]
    fn validate_rejects_empty() {
        assert!(matches!(validate_slot_name(""), Err(SlotError::EmptyName)));
    }

    #[test]
    fn validate_rejects_leading_dash() {
        // Leading dash collides with clap's flag parser.
        assert!(matches!(
            validate_slot_name("-foo"),
            Err(SlotError::InvalidCharacters(_))
        ));
    }

    #[test]
    fn validate_rejects_trailing_dash() {
        // Trailing dashes get silently stripped by common copy-paste pipelines.
        assert!(matches!(
            validate_slot_name("foo-"),
            Err(SlotError::InvalidCharacters(_))
        ));
    }

    #[test]
    fn validate_rejects_too_long() {
        let n = "a".repeat(33);
        assert!(matches!(
            validate_slot_name(&n),
            Err(SlotError::NameTooLong { .. })
        ));
        let max = "b".repeat(32);
        assert!(validate_slot_name(&max).is_ok());
    }

    #[test]
    fn validate_rejects_reserved() {
        for r in RESERVED_SLOT_NAMES {
            assert!(
                matches!(validate_slot_name(r), Err(SlotError::Reserved(_))),
                "{r} should be reserved"
            );
        }
    }

    #[test]
    fn validate_allows_default_keyword() {
        // `default` is NOT in RESERVED_SLOT_NAMES even though migration claims
        // it. Explicit `cqs slot create default` is allowed for parity.
        assert!(validate_slot_name(DEFAULT_SLOT).is_ok());
    }

    // ── path helpers ─────────────────────────────────────────────────────

    #[test]
    fn slot_dir_paths() {
        let cqs = Path::new("/proj/.cqs");
        assert_eq!(
            slot_dir(cqs, "default"),
            Path::new("/proj/.cqs/slots/default")
        );
        assert_eq!(slot_dir(cqs, "e5"), Path::new("/proj/.cqs/slots/e5"));
        assert_eq!(slots_root(cqs), Path::new("/proj/.cqs/slots"));
        assert_eq!(active_slot_path(cqs), Path::new("/proj/.cqs/active_slot"));
    }

    // ── active_slot read / write ─────────────────────────────────────────

    #[test]
    fn read_active_slot_missing_returns_none() {
        let dir = TempDir::new().unwrap();
        assert!(read_active_slot(dir.path()).is_none());
    }

    #[test]
    fn write_then_read_round_trip() {
        let dir = TempDir::new().unwrap();
        write_active_slot(dir.path(), "e5").unwrap();
        assert_eq!(read_active_slot(dir.path()).as_deref(), Some("e5"));
    }

    #[test]
    fn read_handles_corrupt_active_slot() {
        let dir = TempDir::new().unwrap();
        let path = active_slot_path(dir.path());
        // Write garbage bytes (non-UTF8 + invalid even if UTF8 was OK).
        fs::write(&path, b"NOT A VALID slot \xFF\xFE").unwrap();
        assert!(read_active_slot(dir.path()).is_none());
    }

    #[test]
    fn write_rejects_invalid_name() {
        let dir = TempDir::new().unwrap();
        assert!(write_active_slot(dir.path(), "BadName").is_err());
        assert!(write_active_slot(dir.path(), "active").is_err()); // reserved
    }

    // ── list_slots ───────────────────────────────────────────────────────

    #[test]
    fn list_slots_empty_when_dir_missing() {
        let dir = TempDir::new().unwrap();
        let names = list_slots(dir.path()).unwrap();
        assert!(names.is_empty());
    }

    #[test]
    fn list_slots_returns_sorted_names() {
        let dir = TempDir::new().unwrap();
        let cqs = dir.path();
        for n in ["e5", "default", "bge"] {
            fs::create_dir_all(slot_dir(cqs, n)).unwrap();
        }
        // Random non-slot dir we should ignore.
        fs::create_dir_all(slots_root(cqs).join("CamelCase")).unwrap();
        let names = list_slots(cqs).unwrap();
        assert_eq!(names, vec!["bge", "default", "e5"]);
    }

    // ── resolve_slot_name ────────────────────────────────────────────────

    #[test]
    fn resolve_flag_wins() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = TempDir::new().unwrap();
        write_active_slot(dir.path(), "fromfile").unwrap();
        std::env::set_var("CQS_SLOT", "fromenv");
        let r = resolve_slot_name(Some("fromflag"), dir.path()).unwrap();
        std::env::remove_var("CQS_SLOT");
        assert_eq!(r.name, "fromflag");
        assert_eq!(r.source, SlotSource::Flag);
    }

    #[test]
    fn resolve_env_when_no_flag() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = TempDir::new().unwrap();
        write_active_slot(dir.path(), "fromfile").unwrap();
        std::env::set_var("CQS_SLOT", "fromenv");
        let r = resolve_slot_name(None, dir.path()).unwrap();
        std::env::remove_var("CQS_SLOT");
        assert_eq!(r.name, "fromenv");
        assert_eq!(r.source, SlotSource::Env);
    }

    #[test]
    fn resolve_file_when_no_flag_no_env() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = TempDir::new().unwrap();
        write_active_slot(dir.path(), "fromfile").unwrap();
        std::env::remove_var("CQS_SLOT");
        let r = resolve_slot_name(None, dir.path()).unwrap();
        assert_eq!(r.name, "fromfile");
        assert_eq!(r.source, SlotSource::File);
    }

    #[test]
    fn resolve_fallback_default() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = TempDir::new().unwrap();
        std::env::remove_var("CQS_SLOT");
        let r = resolve_slot_name(None, dir.path()).unwrap();
        assert_eq!(r.name, DEFAULT_SLOT);
        assert_eq!(r.source, SlotSource::Fallback);
    }

    #[test]
    fn resolve_rejects_invalid_flag() {
        let dir = TempDir::new().unwrap();
        let err = resolve_slot_name(Some("BAD"), dir.path()).unwrap_err();
        assert!(matches!(err, SlotError::InvalidCharacters(_)));
    }

    // ── migrate_legacy_index_to_default_slot ─────────────────────────────

    #[test]
    fn migrate_noop_when_no_legacy() {
        let dir = TempDir::new().unwrap();
        let cqs = dir.path().join(".cqs");
        fs::create_dir_all(&cqs).unwrap();
        let did = migrate_legacy_index_to_default_slot(&cqs).unwrap();
        assert!(!did);
        assert!(!slots_root(&cqs).exists());
    }

    #[test]
    fn migrate_moves_legacy_to_default() {
        let dir = TempDir::new().unwrap();
        let cqs = dir.path().join(".cqs");
        fs::create_dir_all(&cqs).unwrap();
        // Plant fake artifacts.
        fs::write(cqs.join("index.db"), b"db-data").unwrap();
        fs::write(cqs.join("index.hnsw.data"), b"hnsw-data").unwrap();
        fs::write(cqs.join("index.hnsw.graph"), b"hnsw-graph").unwrap();
        fs::write(cqs.join("splade.index.bin"), b"splade").unwrap();

        let did = migrate_legacy_index_to_default_slot(&cqs).unwrap();
        assert!(did);

        // Now slots/default/ contains them
        let default = slot_dir(&cqs, DEFAULT_SLOT);
        assert!(default.join("index.db").exists());
        assert!(default.join("index.hnsw.data").exists());
        assert!(default.join("index.hnsw.graph").exists());
        assert!(default.join("splade.index.bin").exists());

        // Originals gone
        assert!(!cqs.join("index.db").exists());
        assert!(!cqs.join("index.hnsw.data").exists());
        assert!(!cqs.join("splade.index.bin").exists());

        // Active slot pointer is in place
        assert_eq!(read_active_slot(&cqs).as_deref(), Some(DEFAULT_SLOT));
    }

    #[test]
    fn migrate_idempotent_on_second_run() {
        let dir = TempDir::new().unwrap();
        let cqs = dir.path().join(".cqs");
        fs::create_dir_all(&cqs).unwrap();
        fs::write(cqs.join("index.db"), b"db-data").unwrap();

        assert!(migrate_legacy_index_to_default_slot(&cqs).unwrap());
        // Second run: slots/ exists, must skip cleanly.
        assert!(!migrate_legacy_index_to_default_slot(&cqs).unwrap());
    }

    #[test]
    fn migrate_skipped_when_slots_already_present() {
        let dir = TempDir::new().unwrap();
        let cqs = dir.path().join(".cqs");
        fs::create_dir_all(slots_root(&cqs).join("foo")).unwrap();
        // Plant a legacy file too — should NOT be moved (slots/ already there).
        fs::write(cqs.join("index.db"), b"db-data").unwrap();
        assert!(!migrate_legacy_index_to_default_slot(&cqs).unwrap());
        assert!(cqs.join("index.db").exists());
    }

    /// P2.31 / P2.34: a migration that crashed mid-flight leaves the
    /// `migration.lock` sentinel behind. The next call must refuse to
    /// proceed and surface the failure context for manual recovery —
    /// rather than silently re-running the migration over a partially
    /// migrated tree.
    #[test]
    fn migrate_refuses_to_proceed_when_sentinel_exists() {
        let dir = TempDir::new().unwrap();
        let cqs = dir.path().join(".cqs");
        fs::create_dir_all(&cqs).unwrap();
        fs::write(cqs.join("index.db"), b"db-data").unwrap();

        // Plant the sentinel as if a previous call crashed mid-flight.
        let sentinel = cqs.join(MIGRATION_SENTINEL_FILE);
        fs::write(
            &sentinel,
            "started_at=2026-04-26T00:00:00Z\nstate=in_progress\n",
        )
        .unwrap();

        let err = migrate_legacy_index_to_default_slot(&cqs).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("previous migration failed") || msg.contains(MIGRATION_SENTINEL_FILE),
            "error must surface sentinel-aware recovery context, got: {msg}",
        );

        // Sentinel must persist — only manual recovery removes it.
        assert!(
            sentinel.exists(),
            "sentinel must remain in place until operator removes it"
        );
        // Legacy file must be untouched.
        assert!(cqs.join("index.db").exists());
        // No partial migration: slots/ must not have appeared.
        assert!(!slots_root(&cqs).exists());
    }

    /// P2.31: a successful migration removes the sentinel as the last
    /// step, so subsequent calls can proceed (idempotent no-op via the
    /// `slots/` check).
    #[test]
    fn migrate_clears_sentinel_on_full_success() {
        let dir = TempDir::new().unwrap();
        let cqs = dir.path().join(".cqs");
        fs::create_dir_all(&cqs).unwrap();
        fs::write(cqs.join("index.db"), b"db-data").unwrap();

        assert!(migrate_legacy_index_to_default_slot(&cqs).unwrap());
        // Sentinel must NOT linger after success — the next call would
        // refuse to proceed if it did.
        assert!(
            !cqs.join(MIGRATION_SENTINEL_FILE).exists(),
            "sentinel must be removed on full success"
        );
    }

    // ── slot.toml read / write (#1107) ───────────────────────────────────

    #[test]
    fn read_slot_model_returns_none_when_missing() {
        let dir = TempDir::new().unwrap();
        let cqs = dir.path().join(".cqs");
        fs::create_dir_all(slot_dir(&cqs, "e5")).unwrap();
        assert!(read_slot_model(&cqs, "e5").is_none());
    }

    #[test]
    fn write_then_read_slot_model_round_trip() {
        let dir = TempDir::new().unwrap();
        let cqs = dir.path().join(".cqs");
        fs::create_dir_all(slot_dir(&cqs, "coderank")).unwrap();
        write_slot_model(&cqs, "coderank", "nomic-coderank").unwrap();
        assert_eq!(
            read_slot_model(&cqs, "coderank").as_deref(),
            Some("nomic-coderank")
        );
    }

    #[test]
    fn write_slot_model_preserves_hf_repo_form() {
        let dir = TempDir::new().unwrap();
        let cqs = dir.path().join(".cqs");
        write_slot_model(&cqs, "bge", "BAAI/bge-large-en-v1.5").unwrap();
        assert_eq!(
            read_slot_model(&cqs, "bge").as_deref(),
            Some("BAAI/bge-large-en-v1.5")
        );
    }

    #[test]
    fn write_slot_model_creates_dir_if_missing() {
        let dir = TempDir::new().unwrap();
        let cqs = dir.path().join(".cqs");
        // Note: slot dir does not exist beforehand
        write_slot_model(&cqs, "fresh", "e5-base").unwrap();
        assert!(slot_dir(&cqs, "fresh").exists());
        assert_eq!(read_slot_model(&cqs, "fresh").as_deref(), Some("e5-base"));
    }

    #[test]
    fn read_slot_model_returns_none_on_malformed_toml() {
        let dir = TempDir::new().unwrap();
        let cqs = dir.path().join(".cqs");
        let s = slot_dir(&cqs, "e5");
        fs::create_dir_all(&s).unwrap();
        fs::write(s.join(SLOT_CONFIG_FILE), b"not = valid = toml\n").unwrap();
        assert!(read_slot_model(&cqs, "e5").is_none());
    }

    #[test]
    fn write_slot_model_rejects_invalid_slot_name() {
        let dir = TempDir::new().unwrap();
        let cqs = dir.path().join(".cqs");
        assert!(write_slot_model(&cqs, "BadName", "bge-large").is_err());
    }

    #[test]
    fn write_slot_model_overwrites_previous_value() {
        let dir = TempDir::new().unwrap();
        let cqs = dir.path().join(".cqs");
        write_slot_model(&cqs, "x", "bge-large").unwrap();
        write_slot_model(&cqs, "x", "e5-base").unwrap();
        assert_eq!(read_slot_model(&cqs, "x").as_deref(), Some("e5-base"));
    }

    /// EX-V1.30.1-4 (#1217): the round-trip preserves unrelated top-level
    /// sections. Pre-fix the function clobbered the file with a single
    /// `[embedding]\nmodel = …` block, so any user-added or future-typed
    /// section disappeared on the next `cqs slot promote`. With the
    /// `#[serde(flatten)] extra: toml::Table` catch-all the unknown
    /// section survives verbatim.
    #[test]
    fn write_slot_model_preserves_unrelated_sections() {
        let dir = TempDir::new().unwrap();
        let cqs = dir.path().join(".cqs");
        let slot = "rounds";
        // Hand-write a slot.toml with an embedding model AND a section the
        // current code knows nothing about. Mirrors the issue's example
        // (`[notes].project_id = "foo"`).
        let cfg_path = slot_config_path(&cqs, slot);
        fs::create_dir_all(cfg_path.parent().unwrap()).unwrap();
        let initial =
            "[embedding]\nmodel = \"bge-large\"\n\n[notes]\nproject_id = \"foo\"\nrelease = 7\n";
        fs::write(&cfg_path, initial).unwrap();

        // Mutate via the public API.
        write_slot_model(&cqs, slot, "nomic-coderank").unwrap();

        // Verify: model swapped, notes section still present.
        assert_eq!(
            read_slot_model(&cqs, slot).as_deref(),
            Some("nomic-coderank"),
            "embedding.model was rewritten"
        );
        let raw = fs::read_to_string(&cfg_path).unwrap();
        assert!(
            raw.contains("[notes]"),
            "[notes] section should survive the round-trip; got:\n{raw}"
        );
        assert!(
            raw.contains("project_id"),
            "[notes].project_id should survive; got:\n{raw}"
        );
        assert!(
            raw.contains("release"),
            "[notes].release should survive; got:\n{raw}"
        );
    }

    /// EX-V1.30.1-4 (#1217): malformed slot.toml on disk recovers via
    /// rewrite-from-default rather than erroring the write path. Means a
    /// hand-broken slot.toml can't deadlock `cqs slot promote` — pinning
    /// the tolerance contract documented in the function's doc comment.
    #[test]
    fn write_slot_model_recovers_from_malformed_existing() {
        let dir = TempDir::new().unwrap();
        let cqs = dir.path().join(".cqs");
        let slot = "broken";
        let cfg_path = slot_config_path(&cqs, slot);
        fs::create_dir_all(cfg_path.parent().unwrap()).unwrap();
        // Not valid TOML — unclosed string, dangling bracket.
        fs::write(&cfg_path, "[embedding\nmodel = \"oops").unwrap();

        // Should succeed (warn + fall back to default).
        write_slot_model(&cqs, slot, "e5-base").unwrap();
        assert_eq!(
            read_slot_model(&cqs, slot).as_deref(),
            Some("e5-base"),
            "rewrite-from-default produced a valid file"
        );
    }

    #[test]
    fn slot_config_path_resolves() {
        assert_eq!(
            slot_config_path(Path::new("/proj/.cqs"), "default"),
            Path::new("/proj/.cqs/slots/default/slot.toml")
        );
    }
}
