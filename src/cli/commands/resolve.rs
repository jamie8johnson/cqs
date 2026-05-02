//! Shared target resolution for CLI commands
//!
//! Delegates to `cqs::resolve_target` in the library crate.

use std::path::Path;

use anyhow::Result;
use cqs::config::{Config, ReferenceConfig};
use cqs::reference::{self, ReferenceIndex};
use cqs::store::{ReadOnly, Store};
use cqs::ResolvedTarget;

/// Find a reference's `ReferenceConfig` by name, returning the user-facing
/// "not found" error consistently used by all reference commands.
fn find_reference_config<'a>(config: &'a Config, name: &str) -> Result<&'a ReferenceConfig> {
    config
        .references
        .iter()
        .find(|r| r.name == name)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Reference '{}' not found. Run 'cqs ref list' to see available references.",
                name
            )
        })
}

/// Resolve a target string to a [`ResolvedTarget`] (CLI wrapper).
///
/// Wraps the library's `resolve_target` with anyhow error conversion.
/// Generic over the store's typestate — resolution is a pure query.
pub fn resolve_target<Mode>(store: &Store<Mode>, target: &str) -> Result<ResolvedTarget> {
    let _span = tracing::info_span!("resolve_target", target).entered();
    Ok(cqs::resolve_target(store, target)?)
}

/// Find a reference index by name from the project config.
///
/// Loads config, loads all references, finds the one matching `name`.
/// Returns an error with a user-friendly message if not found.
pub(crate) fn find_reference(root: &Path, name: &str) -> Result<ReferenceIndex> {
    let _span = tracing::info_span!("find_reference", name).entered();
    let config = Config::load(root);
    // Validate the reference name resolves before paying the cost of loading
    // every reference index from disk.
    find_reference_config(&config, name)?;
    let references = reference::load_references(&config.references);
    references
        .into_iter()
        .find(|r| r.name == name)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Reference '{}' not found. Run 'cqs ref list' to see available references.",
                name
            )
        })
}

/// Resolve a reference name to its database path.
///
/// Loads config, finds the reference, and validates that index.db exists.
fn resolve_reference_db(root: &Path, ref_name: &str) -> Result<std::path::PathBuf> {
    use anyhow::bail;

    let config = Config::load(root);
    let ref_cfg = find_reference_config(&config, ref_name)?;

    // Refs are stored at `~/.local/share/cqs/refs/<name>/` with the DB
    // written directly into that directory (`cmd_ref_add` at
    // `infra/reference.rs:204`: `ref_dir.join(INDEX_DB_FILENAME)`). The
    // ref directory IS the cqs index dir — it does NOT have an outer
    // project `.cqs/` segment. `resolve_index_db` then picks the slot
    // layout (`slots/<active>/index.db`) over the legacy bare-file path,
    // matching the writer's behavior post-#1105.
    //
    // Pre-fix this called `resolve_index_dir(&ref_cfg.path)`, which
    // appended a spurious `.cqs/` segment and then `resolve_index_db`
    // looked at `<ref_dir>/.cqs/slots/default/index.db` — a path the
    // writer never produces. `cqs drift` / `cqs diff` against any newly
    // added ref would error with "no index, run cqs ref update". (#1305)
    let ref_db = cqs::resolve_index_db(&ref_cfg.path);
    if !ref_db.exists() {
        bail!(
            "Reference '{}' has no index at {}. Run 'cqs ref update {}' first.",
            ref_name,
            ref_db.display(),
            ref_name
        );
    }
    Ok(ref_db)
}

/// Resolve a reference name to an opened Store.
///
/// Loads config, finds the reference, checks that index.db exists, and opens the store.
/// Shared logic for `cmd_diff` and `cmd_drift` (and any future commands needing a reference store).
pub(crate) fn resolve_reference_store(root: &Path, ref_name: &str) -> Result<Store> {
    use anyhow::Context;
    let ref_db = resolve_reference_db(root, ref_name)?;
    Store::open(&ref_db)
        .with_context(|| format!("Failed to open reference store at {}", ref_db.display()))
}

/// Like [`resolve_reference_store`] but opens the store in read-only mode.
pub(crate) fn resolve_reference_store_readonly(
    root: &Path,
    ref_name: &str,
) -> Result<Store<ReadOnly>> {
    use anyhow::Context;
    let ref_db = resolve_reference_db(root, ref_name)?;
    Store::open_readonly(&ref_db)
        .with_context(|| format!("Failed to open reference store at {}", ref_db.display()))
}
