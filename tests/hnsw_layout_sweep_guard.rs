//! Completeness guard: the HNSW persistence layout is manifest-generation, and
//! the retired five-file layout is known to EXACTLY two legacy/migration sites.
//!
//! # Background
//!
//! The HNSW on-disk format changed from a fixed five-file set
//! (`{basename}.hnsw.{graph,data,ids,meta,checksum}`) to generation-suffixed
//! artifacts (`{basename}.gen<N>.hnsw.{graph,data,ids}`) behind an atomically
//! committed `{basename}.hnsw.manifest` pointer. The `meta` and `checksum`
//! sidecars no longer exist: the distance metric and the per-artifact blake3
//! checksums are now inlined into the manifest.
//!
//! That was a *sweep*: every site that enumerated, sized, existence-checked,
//! GC'd, or fabricated the old layout had to move to resolving the current
//! generation via the manifest (`HnswIndex::artifact_paths` / `exists` /
//! `purge_artifacts`). A sweep that updates N-1 of N sites leaves the survivor
//! green in isolation — it still believes the old files exist — while the
//! feature (or the cleanup, or the size report) is silently wrong.
//!
//! # The property this guard pins
//!
//! The two retired file extensions `hnsw.meta` and `hnsw.checksum` are a clean
//! discriminator for old-layout knowledge: they exist ONLY in the retired
//! scheme. (The three carried-over extensions `hnsw.{graph,data,ids}` cannot be
//! banned — they still appear, correctly, as `format!("{gen_base}.hnsw.graph")`
//! everywhere in the live save/load path.) So the invariant is:
//!
//!   *A `hnsw.meta` / `hnsw.checksum` literal may appear in `src/` ONLY inside
//!    the two deliberate legacy/migration sites.*
//!
//! - `src/hnsw/persist.rs` — `LEGACY_HNSW_EXTENSIONS` (the legacy-GC list that
//!   DELETES a pre-manifest five-file set under the exclusive save lock) plus
//!   the retired-scheme regression tests that fabricate `.hnsw.checksum` /
//!   `.bak` debris to prove a save is no longer wedged by it.
//! - `src/slot/mod.rs` — `collect_migration_files` (the deliberate survivor:
//!   the pre-slot `.cqs/` -> `slots/default/` migration only ever sees legacy
//!   sets, since generation artifacts + manifests are born inside slot dirs).
//!
//! Any OTHER `src/` file carrying one of these literals is an un-swept
//! straggler: a site that still believes the retired layout is on disk. It
//! would pass its own unit tests (each member is correct in isolation) while
//! diverging from the swept majority — the exact null a per-unit suite cannot
//! express.
//!
//! # Why a source-text scan, not a value/behavior test
//!
//! The divergence is *structural* — a filename literal that names files that no
//! longer exist. A behavioral test of any single member is green (the straggler
//! does the old thing correctly). Only a relation over the whole `src/` tree —
//! "these literals appear nowhere but the two legacy sites" — expresses it. When
//! the next layout change lands, a member left on `hnsw.meta`/`hnsw.checksum`
//! fails HERE with its file:line, before it can strand silently.
//!
//! Status at authoring: GREEN. The layout sweep was verified complete across
//! every code member (persist core + legacy allowlist, `exists` /
//! `artifact_paths` / `purge_artifacts`, `stats.rs`, `model.rs` size summing,
//! `gc.rs`, the `store.rs` dirty-flag tests, the daemon staleness path which
//! keys on `index.db` not the HNSW files, `reference.rs` ref-copy which rebuilds
//! fresh + chmods generation-agnostically, and the integration tests). This
//! guard is hardening: it makes the NEXT layout change unable to strand a code
//! member. Its RED behavior is calibrated by injection — a retired literal added
//! to any non-allowlisted `src/` file names that file and fails.

use std::path::{Path, PathBuf};

/// The retired five-file-layout extensions. Present ONLY in the old scheme —
/// the manifest inlines the metric (was `hnsw.meta`) and the checksums (was
/// `hnsw.checksum`), so a live reference to either is old-layout knowledge.
const RETIRED_EXTENSIONS: &[&str] = &["hnsw.meta", "hnsw.checksum"];

/// The two `src/` files where a retired-extension literal is legitimate: the
/// legacy-GC list and the pre-slot migration list. Paths are relative to the
/// crate root, forward-slashed (paths are normalized before comparison so the
/// guard is portable across Windows/Unix separators).
const LEGACY_ALLOWLIST: &[&str] = &["src/hnsw/persist.rs", "src/slot/mod.rs"];

/// Named anchors that MUST still exist inside each legacy site, so a rename of
/// `LEGACY_HNSW_EXTENSIONS` / `collect_migration_files` can't silently drop a
/// family member from the enumerated set (the "site moved" guard).
const REQUIRED_ANCHORS: &[(&str, &str)] = &[
    ("src/hnsw/persist.rs", "LEGACY_HNSW_EXTENSIONS"),
    ("src/slot/mod.rs", "fn collect_migration_files"),
];

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn collect_rs(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_file() {
        if path.extension().map(|x| x == "rs").unwrap_or(false) {
            out.push(path.to_path_buf());
        }
        return;
    }
    if let Ok(rd) = std::fs::read_dir(path) {
        for entry in rd.flatten() {
            collect_rs(&entry.path(), out);
        }
    }
}

/// Crate-root-relative, forward-slashed path for a `src/` file.
fn rel(path: &Path) -> String {
    let root = crate_root();
    path.strip_prefix(&root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// The retired extension a line references, if any (`None` for clean lines).
fn retired_hit(line: &str) -> Option<&'static str> {
    RETIRED_EXTENSIONS
        .iter()
        .copied()
        .find(|ext| line.contains(ext))
}

/// (1) The sweep guard: no retired-extension literal outside the two legacy
/// sites. RED with the offending `file:line` when the next layout change leaves
/// a member behind.
#[test]
fn no_retired_hnsw_layout_literals_outside_legacy_allowlist() {
    let src = crate_root().join("src");
    let mut files = Vec::new();
    collect_rs(&src, &mut files);
    assert!(
        !files.is_empty(),
        "site-moved guard: found zero .rs files under src/ — the scan root moved; \
         re-point `collect_rs` before trusting a GREEN result"
    );

    let mut stragglers: Vec<String> = Vec::new();
    for file in &files {
        let relpath = rel(file);
        if LEGACY_ALLOWLIST.contains(&relpath.as_str()) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        for (i, line) in text.lines().enumerate() {
            if let Some(ext) = retired_hit(line) {
                stragglers.push(format!(
                    "{relpath}:{} references retired HNSW extension `{ext}` — \
                     this site still believes the five-file layout exists; \
                     port it to the manifest/generation layout \
                     (HnswIndex::artifact_paths / exists / purge_artifacts). Line: {}",
                    i + 1,
                    line.trim()
                ));
            }
        }
    }

    assert!(
        stragglers.is_empty(),
        "HNSW layout sweep is incomplete — {} straggler(s) still name the retired \
         five-file layout outside the legacy/migration allowlist:\n{}",
        stragglers.len(),
        stragglers.join("\n")
    );
}

/// (2) Anti-stale allowlist: each allowlisted file must STILL carry a retired
/// literal. If it does not, the legacy site was removed or moved and the
/// allowlist entry is dead — a refactor could then re-add a straggler under the
/// stale blessing without tripping test (1). Fail so the allowlist is pruned.
#[test]
fn legacy_allowlist_entries_are_live() {
    for entry in LEGACY_ALLOWLIST {
        let path = crate_root().join(entry);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("allowlisted file {entry} unreadable: {e}"));
        let has_literal = text.lines().any(|l| retired_hit(l).is_some());
        assert!(
            has_literal,
            "allowlist entry `{entry}` no longer contains any retired HNSW extension \
             literal — the legacy/migration site moved. Remove this entry (and re-audit \
             whether the site's old-layout knowledge migrated somewhere unguarded)."
        );
    }
}

/// (3) Site-moved guard: the named anchors inside each legacy site still exist,
/// so a rename of `LEGACY_HNSW_EXTENSIONS` / `collect_migration_files` can't
/// silently drop a family member from the enumerated set.
#[test]
fn legacy_site_anchors_still_present() {
    for (file, anchor) in REQUIRED_ANCHORS {
        let path = crate_root().join(file);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("anchored file {file} unreadable: {e}"));
        assert!(
            text.contains(anchor),
            "site-moved guard: `{anchor}` not found in {file} — the legacy site was \
             renamed or removed. Update REQUIRED_ANCHORS / LEGACY_ALLOWLIST after \
             re-auditing where its old-layout handling now lives."
        );
    }
}

/// (4) Positive layout anchor: the current manifest-generation format is still
/// in force (the manifest pointer + a generation-suffixed artifact basename).
/// A wholesale revert to a fixed-name scheme would drop these and fail here,
/// keeping the guard's premise (retired == meta/checksum only) honest.
#[test]
fn manifest_generation_layout_is_in_force() {
    let persist = crate_root().join("src/hnsw/persist.rs");
    let text = std::fs::read_to_string(&persist).expect("src/hnsw/persist.rs unreadable");
    assert!(
        text.contains("hnsw.manifest"),
        "manifest pointer literal `hnsw.manifest` absent from persist.rs — the \
         layout premise of this guard no longer holds; re-derive the retired set"
    );
    assert!(
        text.contains("gen_basename") || text.contains(".gen{"),
        "generation-suffixed basename construction absent from persist.rs — the \
         guard assumes generation artifacts; re-derive the retired set"
    );
}
