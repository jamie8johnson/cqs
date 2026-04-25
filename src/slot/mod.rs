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
use std::io::Write;
use std::path::{Path, PathBuf};

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
    if RESERVED_SLOT_NAMES.contains(&name) {
        return Err(SlotError::Reserved(name.to_string()));
    }
    Ok(())
}

/// Path of `.cqs/slots/<name>/` for the given project `.cqs/` dir + slot name.
pub fn slot_dir(project_cqs_dir: &Path, slot_name: &str) -> PathBuf {
    project_cqs_dir.join(SLOTS_DIR).join(slot_name)
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
    let raw = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "Failed to read slot config; falling back to default resolution"
            );
            return None;
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
/// Atomic via temp+rename. Creates the slot dir if missing (idempotent).
/// Existing TOML keys outside `[embedding]` are not preserved — slot.toml is
/// owned by cqs; users should not hand-edit it.
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
    let tmp_path = dir.join(format!("{}.tmp", SLOT_CONFIG_FILE));

    // Hand-write the body so unrelated TOML keys (if a user added some) don't
    // get clobbered through serde round-tripping. With only one section this
    // is simpler than a Document-preserving edit.
    let body = format!("[embedding]\nmodel = {}\n", toml_quote(model));
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
        f.sync_all().map_err(|source| SlotError::Io {
            slot: slot_name.to_string(),
            source,
        })?;
    }
    fs::rename(&tmp_path, &final_path).map_err(|source| SlotError::Io {
        slot: slot_name.to_string(),
        source,
    })?;
    Ok(())
}

#[derive(serde::Deserialize)]
struct SlotConfigFile {
    embedding: Option<SlotEmbeddingSection>,
}

#[derive(serde::Deserialize)]
struct SlotEmbeddingSection {
    model: Option<String>,
}

/// Quote a value for use as a TOML basic string. Escapes the bare minimum
/// (`\`, `"`, control chars) so a preset name like `BAAI/bge-large-en-v1.5`
/// round-trips cleanly. Slot names are pre-validated (a-z, 0-9, _, -) so the
/// only risky characters live in the model value.
fn toml_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
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
    match fs::read_to_string(&path) {
        Ok(s) => {
            let trimmed = s.trim();
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
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "Failed to read active_slot file; falling back to default"
            );
            None
        }
    }
}

/// Write the active slot pointer atomically. Validates the name first.
///
/// Writes to a sibling `<active_slot>.tmp` then `rename`s into place — atomic
/// on the same filesystem. Crash between write and rename leaves the previous
/// pointer intact.
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
    let tmp_path = project_cqs_dir.join(format!("{}.tmp", ACTIVE_SLOT_FILE));

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
        // Best-effort fsync before rename so the rename atomicity covers a
        // populated file, not an empty one. Failures here are non-fatal — the
        // fsync is belt-and-braces over `rename`'s own crash safety.
        if let Err(e) = f.sync_all() {
            tracing::debug!(error = %e, "active_slot tmp fsync failed (non-fatal)");
        }
    }

    fs::rename(&tmp_path, &final_path).map_err(|source| SlotError::Io {
        slot: slot_name.to_string(),
        source,
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

/// One-shot filesystem migration: move legacy `.cqs/index.db` (and its HNSW /
/// SPLADE sidecars) into `.cqs/slots/default/`, then write
/// `.cqs/active_slot = "default"`.
///
/// Idempotent: if `.cqs/slots/` already exists, this is a no-op. Atomic where
/// the source and destination live on the same filesystem (common case);
/// otherwise falls back to copy + delete with an inventory-based rollback on
/// partial failure.
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
            for (already_dst, already_src) in moved.iter().rev() {
                if let Err(rollback_err) = move_file(already_dst, already_src) {
                    tracing::error!(
                        src = %already_dst.display(),
                        dst = %already_src.display(),
                        error = %rollback_err,
                        "rollback failed (manual recovery may be needed)"
                    );
                }
            }
            // Best-effort: clean up the empty slots/default/ + slots/ if rollback was clean.
            let _ = fs::remove_dir(&dest);
            let _ = fs::remove_dir(&slots_dir);
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
    tracing::info!(
        files_moved = moved.len(),
        from = %project_cqs_dir.display(),
        to = %dest.display(),
        "legacy index.db migrated to slots/default/"
    );
    Ok(true)
}

/// Collect every file we want to migrate from `.cqs/` → `.cqs/slots/default/`.
/// Hardcoded list of patterns since there's no manifest of "slot-local" files
/// in the rest of the codebase.
fn collect_migration_files(project_cqs_dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    // Always-present
    let candidates = [
        crate::INDEX_DB_FILENAME,
        "index.db-wal",
        "index.db-shm",
        "index.db.bak",
        // HNSW (enriched + base, persistence + index)
        "index.hnsw.data",
        "index.hnsw.graph",
        "index_base.hnsw.data",
        "index_base.hnsw.graph",
        "index.hnsw.lock",
        "index.cagra",
        "index.cagra.sidecar",
        // SPLADE
        "splade.index.bin",
    ];
    for name in candidates {
        let p = project_cqs_dir.join(name);
        if p.exists() {
            out.push(p);
        }
    }
    out
}

/// Move a file, atomic where possible; falls back to copy + remove for
/// cross-device renames. Errors surface to caller for inventory-based rollback.
fn move_file(src: &Path, dst: &Path) -> std::io::Result<()> {
    match fs::rename(src, dst) {
        Ok(()) => Ok(()),
        Err(e) if e.raw_os_error() == Some(libc_exdev()) => {
            fs::copy(src, dst)?;
            fs::remove_file(src)?;
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// EXDEV `errno` value (cross-device link). We hardcode 18 (Linux) since
/// `libc::EXDEV` would pull in a libc dep just for this constant. macOS also
/// uses 18; Windows doesn't surface EXDEV the same way (rename across
/// filesystems just succeeds via the win32 API).
#[inline]
fn libc_exdev() -> i32 {
    18
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

    #[test]
    fn slot_config_path_resolves() {
        assert_eq!(
            slot_config_path(Path::new("/proj/.cqs"), "default"),
            Path::new("/proj/.cqs/slots/default/slot.toml")
        );
    }
}
