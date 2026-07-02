//! Discipline guard: prevent JSON-noise re-decay.
//!
//! The SNR-restoration audit (`docs/json-snr-restoration.md`, historical
//! design in `docs/json-noise-audit.md`) de-noised cqs's agent-facing JSON by
//! applying `#[serde(skip_serializing_if = ...)]` where a field's default value
//! carries no information beyond its own absence. Without an enforcement
//! mechanism, a newly-added `Option<T>` field defaults to always-emitting
//! `"field": null` and silently rebuilds the noise the audit removed. This test
//! is that enforcement mechanism.
//!
//! # What it checks
//!
//! Every `Option<T>` field on a `Serialize`-deriving struct in the in-scope
//! JSON-emission modules must carry EITHER:
//!   - `#[serde(skip_serializing_if = ...)]` / `#[serde(skip)]` / `#[serde(flatten)]`
//!     (the field is omitted when it carries no information), OR
//!   - an explicit `// always-emit: <reason>` marker comment in the field's
//!     preamble (the field is deliberately present even when `None`, because
//!     its `null` value is load-bearing — an envelope contract field, or a
//!     "we checked and it is not applicable" signal, or a uniform-shape result
//!     key agents parse without existence checks).
//!
//! An unannotated `Option<T>` fails the guard: the author must make an explicit
//! skip-or-always-emit decision, which is exactly the discipline that stops the
//! noise from re-accreting.
//!
//! # Scope rule (stated so silent scope-shrink is visible)
//!
//! In scope: `Serialize`-deriving, non-`#[cfg(test)]` structs defined under
//! `src/cli/commands/**`, `src/cli/batch/**`, plus `src/cli/json_envelope.rs`
//! and `src/cli/display.rs` — the CLI / batch / daemon JSON-emission layer and
//! its shared envelope + search-output types. The test enumerates every file it
//! scans and asserts a floor count plus the presence of anchor files, so a glob
//! that silently stops matching (a moved/renamed module) fails the guard rather
//! than passing vacuously.
//!
//! Deliberately OUT of scope, with rationale:
//!   - **Field types other than `Option<T>`.** `Vec`/map/set fields serialize
//!     `[]`/`{}` and `String` serializes `""`; those empty states are frequently
//!     load-bearing ("we computed this and it is empty" is distinct from "we did
//!     not compute it" — the audit's precedence rule #3, cf. `score: 0.0`), so
//!     most are legitimately always-emit and mandating a marker on every one
//!     would bury the signal. `Option<T>` is the one category where `null` and
//!     absence are near-synonyms, i.e. the canonical noise vector. Extending the
//!     guard to collections is a possible follow-up.
//!   - **Lib payload types** (`src/gather.rs`, `src/impact/`, `src/review.rs`,
//!     `src/store/helpers/types.rs`, ...). They are the actual serialized
//!     payloads but reachability from the command adapters is not resolved here;
//!     extending the walker to them is a possible follow-up.
//!   - **Persistence / API-request / web-viz types** (`src/config.rs`,
//!     `src/note.rs`, `src/eval/schema.rs`, `src/llm/`, `src/serve/data.rs`).
//!     These derive `Serialize` for on-disk formats, outbound Anthropic API
//!     bodies, or the SPLADE-viz web UI — different contracts where
//!     skip-when-default can be semantically wrong.

use std::path::{Path, PathBuf};
use syn::{Attribute, Fields, Item, ItemStruct, Meta, Type};

/// Files and directories that make up the in-scope JSON-emission surface,
/// relative to the crate root. Directories are walked recursively.
const SCOPE: &[&str] = &[
    "src/cli/commands",
    "src/cli/batch",
    "src/cli/json_envelope.rs",
    "src/cli/display.rs",
];

/// Anchor files that must always be in the scanned set. If any of these stops
/// being discovered (moved, renamed, or the glob broke), the guard fails loudly
/// instead of silently shrinking its coverage.
const ANCHORS: &[&str] = &[
    "src/cli/json_envelope.rs",
    "src/cli/display.rs",
    "src/cli/commands/index/stats.rs",
    "src/cli/commands/infra/doctor.rs",
    "src/cli/commands/search/query.rs",
];

/// Floor on the number of files scanned. Well below the current count; a drop
/// past this means the scope glob broke.
const MIN_FILES_SCANNED: usize = 40;

/// Floor on the number of in-scope `Option<T>` fields the walker classifies
/// (marked-or-skipped). If detection silently breaks (e.g. stops recognizing
/// the fully-qualified `#[derive(serde::Serialize)]` form), this count collapses
/// and the guard fails rather than passing vacuously.
const MIN_OPTION_FIELDS_SEEN: usize = 40;

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

/// `#[derive(...)]` includes `Serialize` (bare `Serialize`, `serde::Serialize`,
/// or `serde_derive::Serialize` — matched on the last path segment).
fn derives_serialize(attrs: &[Attribute]) -> bool {
    for a in attrs {
        if a.path().is_ident("derive") {
            let mut found = false;
            let _ = a.parse_nested_meta(|m| {
                if m.path
                    .segments
                    .last()
                    .map(|s| s.ident == "Serialize")
                    .unwrap_or(false)
                {
                    found = true;
                }
                Ok(())
            });
            if found {
                return true;
            }
        }
    }
    false
}

/// A `#[cfg(test)]` / `#[cfg(all(test, ...))]` gate — test-only types are out of
/// scope (they never reach a real agent).
fn has_cfg_test(attrs: &[Attribute]) -> bool {
    for a in attrs {
        if a.path().is_ident("cfg") {
            if let Meta::List(ml) = &a.meta {
                if ml.tokens.to_string().contains("test") {
                    return true;
                }
            }
        }
    }
    false
}

/// The field's serde attributes already omit it when empty / merge it inline
/// (`skip_serializing_if`, `skip`, `skip_serializing`, `flatten`), so the guard
/// is satisfied without an always-emit marker.
fn serde_handles_absence(attrs: &[Attribute]) -> bool {
    for a in attrs {
        if a.path().is_ident("serde") {
            if let Meta::List(ml) = &a.meta {
                let s = ml.tokens.to_string();
                if s.contains("skip_serializing_if")
                    || s.contains("skip_serializing")
                    || s.contains("skip")
                    || s.contains("flatten")
                {
                    return true;
                }
            }
        }
    }
    false
}

/// True when the field's type is `Option<...>` (bare or path-qualified).
fn is_option(ty: &Type) -> bool {
    if let Type::Path(tp) = ty {
        if let Some(seg) = tp.path.segments.last() {
            return seg.ident == "Option";
        }
    }
    false
}

/// Scan upward from the field's identifier line across the contiguous preamble
/// of comments / attributes / blank lines. Returns true if any of them carries
/// the `always-emit` marker (a `// always-emit:` line comment, a `///`
/// doc-comment mentioning it, or a `#[doc = "...always-emit..."]` attribute).
/// `field_line` is 1-based (from a proc-macro2 span with the `span-locations`
/// feature enabled).
fn has_always_emit_marker(lines: &[&str], field_line: usize) -> bool {
    let mut i = field_line as i64 - 2; // 0-based index of the line above the field
    while i >= 0 {
        let l = lines[i as usize].trim();
        let is_preamble = l.is_empty()
            || l.starts_with("//")
            || l.starts_with('#')
            || l.starts_with(')')
            || l.starts_with(']');
        if !is_preamble {
            break;
        }
        if l.contains("always-emit") {
            return true;
        }
        i -= 1;
    }
    false
}

fn flatten_structs(items: &[Item], out: &mut Vec<ItemStruct>) {
    for item in items {
        match item {
            Item::Struct(s) => out.push(s.clone()),
            Item::Mod(m) if !has_cfg_test(&m.attrs) => {
                if let Some((_, inner)) = &m.content {
                    flatten_structs(inner, out);
                }
            }
            _ => {}
        }
    }
}

#[test]
fn json_emitting_option_fields_are_skip_or_marked() {
    let root = crate_root();

    let mut files = Vec::new();
    for entry in SCOPE {
        collect_rs(&root.join(entry), &mut files);
    }
    files.sort();
    files.dedup();

    // Scope-shrink tripwires: the glob must still be finding the surface.
    assert!(
        files.len() >= MIN_FILES_SCANNED,
        "JSON-noise guard scanned only {} files (floor {}); the scope glob \
         likely broke — check {:?}",
        files.len(),
        MIN_FILES_SCANNED,
        SCOPE,
    );
    for anchor in ANCHORS {
        let want = root.join(anchor);
        assert!(
            files.contains(&want),
            "JSON-noise guard lost anchor file {anchor}; a module was moved or \
             renamed and the guard's coverage silently shrank",
        );
    }

    let mut option_fields_seen = 0usize;
    let mut violations: Vec<String> = Vec::new();

    for f in &files {
        let src =
            std::fs::read_to_string(f).unwrap_or_else(|e| panic!("read {}: {e}", f.display()));
        let lines: Vec<&str> = src.lines().collect();
        let ast = syn::parse_file(&src).unwrap_or_else(|e| panic!("parse {}: {e}", f.display()));

        let mut structs = Vec::new();
        flatten_structs(&ast.items, &mut structs);

        for s in &structs {
            if !derives_serialize(&s.attrs) || has_cfg_test(&s.attrs) {
                continue;
            }
            let Fields::Named(named) = &s.fields else {
                continue;
            };
            for field in &named.named {
                if !is_option(&field.ty) {
                    continue;
                }
                option_fields_seen += 1;
                if serde_handles_absence(&field.attrs) {
                    continue;
                }
                let ident = field.ident.as_ref().expect("named field has an identifier");
                let line = ident.span().start().line;
                if has_always_emit_marker(&lines, line) {
                    continue;
                }
                let rel = f.strip_prefix(&root).unwrap_or(f).display();
                violations.push(format!(
                    "  {}: {}::{} (Option field @ line {})",
                    rel, s.ident, ident, line,
                ));
            }
        }
    }

    // Anti-vacuous floor: if detection breaks, the count collapses.
    assert!(
        option_fields_seen >= MIN_OPTION_FIELDS_SEEN,
        "JSON-noise guard saw only {} Option fields (floor {}); struct/field \
         detection likely broke and the guard would pass vacuously",
        option_fields_seen,
        MIN_OPTION_FIELDS_SEEN,
    );

    assert!(
        violations.is_empty(),
        "JSON-noise re-decay guard: {} Option field(s) on a JSON-emitting path \
         neither skip-when-none nor carry an explicit always-emit marker.\n\n\
         Each field below emits `\"<name>\": null` when None, rebuilding the \
         noise the SNR audit removed. Fix each by adding either:\n\
         \x20 #[serde(skip_serializing_if = \"Option::is_none\")]   (if null carries no info)\n\
         \x20 // always-emit: <reason>                              (if null is load-bearing)\n\n\
         Violations:\n{}",
        violations.len(),
        violations.join("\n"),
    );
}
