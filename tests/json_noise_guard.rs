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
//! Every `Option<T>` field on a `Serialize`-deriving struct in the GUARDED
//! modules must carry EITHER:
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
//! # Scope (stated so silent scope-shrink is visible)
//!
//! Two tiers, both anchored so a moved/renamed module fails loudly rather than
//! passing vacuously:
//!
//! 1. **GUARDED** (`GUARDED_SCOPE`) — the `Serialize`-deriving, non-`#[cfg(test)]`
//!    structs whose `Option` fields MUST be skip-or-marked:
//!    - the CLI / batch / daemon JSON-emission layer: `src/cli/commands/**`,
//!      `src/cli/batch/**`, `src/cli/json_envelope.rs`, `src/cli/display.rs`.
//!    - the **lib payload types** that the adapters serialize: `src/gather.rs`,
//!      `src/review.rs`, `src/scout.rs`, `src/health.rs`, `src/impact/**`,
//!      `src/store/helpers/types.rs`. These are the ACTUAL serialized payloads —
//!      the adapters project them through `serde_json::to_value` / lib transforms
//!      (`impact_to_json`) / erasure to `Vec<serde_json::Value>`, so the field
//!      link from an adapter's `*Output` struct is severed at the `to_value`
//!      boundary and a field-reachability walker can't chase it (see the
//!      "why not reachability" note below). The curated list is the honest shape.
//!
//! 2. **A whole-`src/` completeness tripwire.** The walk covers EVERY `.rs` file
//!    under `src/`, not just the GUARDED set. Any unhandled `Serialize` `Option`
//!    OUTSIDE the GUARDED set must have its module listed in `OUT_OF_SCOPE_LEDGER`
//!    with a rationale. A new module (or a currently-clean module that grows a
//!    noisy `Option`) that is neither guarded nor ledgered is a NAMED failure:
//!    "guard it (annotate) or ledger it (document why its null is acceptable)".
//!    This makes the "curated allowlist needs maintenance" cost fail-closed and
//!    checkable — a new lib payload module cannot re-decay the surface invisibly.
//!    The ledger is a deferral record, NOT a blessing: entries name surfaces that
//!    have not yet been through the SNR discipline (or serialize under a different
//!    contract), and each is a candidate for a future coverage extension.
//!
//! # Why the curated allowlist, not type-reachability
//!
//! The proper shape would chase the lib payload types from the command adapters'
//! `*Output` structs. It is not viable here without rust-analyzer-grade type
//! resolution: the payloads reach the wire through paths that sever the field
//! type link — erasure to `serde_json::Value` before landing in the `*Output`
//! struct (`gather`: `GatherOutput.chunks: Vec<serde_json::Value>` built from
//! `to_value(&GatheredChunk)`), lib transform functions rather than field
//! projection (`impact`: `impact_to_json(&ImpactResult)`, which even hand-rolls
//! `impl Serialize`), and direct serialization of the lib type where the
//! adapter's own struct is not the payload (`health`: `to_value(&HealthReport)`).
//! In every case the `*Output` fields name `serde_json::Value`, not the lib type,
//! so chasing them needs local-variable type inference across `use` re-exports.
//! The curated `GUARDED_SCOPE` list plus the whole-`src/` ledger tripwire is the
//! honest middle: enumerated coverage, fail-closed on new modules.
//!
//! # Deliberately OUT of scope (field-type level), with rationale
//!   - **Field types other than `Option<T>`.** `Vec`/map/set fields serialize
//!     `[]`/`{}` and `String` serializes `""`; those empty states are frequently
//!     load-bearing ("we computed this and it is empty" is distinct from "we did
//!     not compute it" — the audit's precedence rule #3, cf. `score: 0.0`), so
//!     most are legitimately always-emit and mandating a marker on every one
//!     would bury the signal. `Option<T>` is the one category where `null` and
//!     absence are near-synonyms, i.e. the canonical noise vector. Extending the
//!     guard to collections is a possible follow-up.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use syn::{Attribute, Fields, Item, ItemStruct, Meta, Type};

/// GUARDED modules whose `Serialize` `Option` fields MUST be skip-or-marked,
/// relative to the crate root. Directories are walked recursively; file entries
/// match exactly (path components, so `gather.rs` never matches `gather_x.rs`).
const GUARDED_SCOPE: &[&str] = &[
    // CLI / batch / daemon JSON-emission layer.
    "src/cli/commands",
    "src/cli/batch",
    "src/cli/json_envelope.rs",
    "src/cli/display.rs",
    // Lib payload types the adapters serialize.
    "src/gather.rs",
    "src/review.rs",
    "src/scout.rs",
    "src/health.rs",
    "src/impact",
    "src/store/helpers/types.rs",
    // Daemon/watch wire types agents deserialize off the socket:
    // `cqs status --watch --json` returns `WatchSnapshot`; `cqs ping`
    // returns `PingResponse`.
    "src/watch_status.rs",
    "src/daemon_translate.rs",
];

/// Modules that serialize a `Serialize` `Option` OUTSIDE the GUARDED set and are
/// deliberately deferred, each with a rationale. This is the completeness
/// tripwire's escape hatch: an unhandled `Option` here is accepted, an unhandled
/// `Option` in an UNlisted, UNguarded module fails the walk. A deferral record,
/// not a blessing — every entry is a candidate for a future coverage extension.
const OUT_OF_SCOPE_LEDGER: &[(&str, &str)] = &[
    // On-disk config format — a persistence contract where skip-when-default can
    // be semantically wrong (an absent key ≠ its default). Named out-of-scope by
    // the SNR audit.
    ("src/config.rs", "on-disk config persistence contract"),
    // Eval fixture schema — on-disk test-data format, not an agent wire shape.
    ("src/eval/schema.rs", "on-disk eval fixture format"),
    // SPLADE-viz web UI payloads — a browser contract, not the agent surface.
    ("src/serve/data.rs", "SPLADE-viz web UI payload"),
    // HNSW sidecar header — on-disk binary index metadata, not agent JSON.
    ("src/hnsw/persist.rs", "on-disk HNSW sidecar header"),
    // Diff lib type — re-projected by `src/cli/commands/io/diff.rs` into its own
    // (GUARDED) `DiffEntryOutput`; the lib derive is not the agent wire shape.
    (
        "src/diff.rs",
        "re-projected into cli::commands::io::diff::DiffEntryOutput (guarded)",
    ),
];

/// Anchor files/dirs that must always be in the scanned set. If any stops being
/// discovered (moved, renamed, or the glob broke), the guard fails loudly
/// instead of silently shrinking its coverage. Covers both GUARDED tiers.
const GUARDED_ANCHORS: &[&str] = &[
    // CLI-layer anchors.
    "src/cli/json_envelope.rs",
    "src/cli/display.rs",
    "src/cli/commands/index/stats.rs",
    "src/cli/commands/infra/doctor.rs",
    "src/cli/commands/search/query.rs",
    // Lib-payload anchors.
    "src/gather.rs",
    "src/review.rs",
    "src/scout.rs",
    "src/health.rs",
    "src/impact/types.rs",
    "src/store/helpers/types.rs",
    // Daemon/watch wire-type anchors.
    "src/watch_status.rs",
    "src/daemon_translate.rs",
];

/// Floor on the number of `.rs` files scanned across `src/`. Well below the
/// current count; a drop past this means the walk broke.
const MIN_FILES_SCANNED: usize = 200;

/// Floor on the number of `Option<T>` fields the whole-`src/` walk classifies
/// (marked-or-skipped-or-violation). If detection silently breaks (e.g. stops
/// recognizing the fully-qualified `#[derive(serde::Serialize)]` form), this
/// count collapses and the guard fails rather than passing vacuously.
const MIN_OPTION_FIELDS_SEEN: usize = 100;

/// Floor on the number of `Option<T>` fields seen inside the GUARDED set
/// specifically. Pins that the GUARDED walk (CLI + the lib payloads) is
/// non-trivially covered — if guarded-struct detection breaks, this collapses.
const MIN_GUARDED_OPTION_FIELDS: usize = 75;

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

/// True when `file` is under one of `scope`'s entries (component-wise prefix, so
/// a file entry matches only its exact path and a dir entry matches its subtree).
fn under_any(file: &Path, root: &Path, scope: &[&str]) -> bool {
    scope.iter().any(|e| file.starts_with(root.join(e)))
}

/// The `OUT_OF_SCOPE_LEDGER` module `file` falls under, if any.
fn ledger_module_of(file: &Path, root: &Path) -> Option<&'static str> {
    OUT_OF_SCOPE_LEDGER
        .iter()
        .find(|(m, _)| file.starts_with(root.join(m)))
        .map(|(m, _)| *m)
}

#[test]
fn json_emitting_option_fields_are_skip_or_marked() {
    let root = crate_root();

    let mut files = Vec::new();
    collect_rs(&root.join("src"), &mut files);
    files.sort();
    files.dedup();

    // Scope-shrink tripwires: the walk must still be finding the surface.
    assert!(
        files.len() >= MIN_FILES_SCANNED,
        "JSON-noise guard scanned only {} files (floor {}); the src/ walk \
         likely broke",
        files.len(),
        MIN_FILES_SCANNED,
    );
    for anchor in GUARDED_ANCHORS {
        let want = root.join(anchor);
        assert!(
            files.contains(&want),
            "JSON-noise guard lost anchor file {anchor}; a module was moved or \
             renamed and the guard's coverage silently shrank",
        );
    }

    let mut option_fields_seen = 0usize;
    let mut guarded_option_fields_seen = 0usize;
    // Unhandled Option in the GUARDED set — must be annotated (primary guard).
    let mut guarded_violations: Vec<String> = Vec::new();
    // Unhandled Option outside GUARDED and not ledgered — must be guarded or
    // ledgered (completeness tripwire).
    let mut unclassified_violations: Vec<String> = Vec::new();
    // Per-ledger-module hit count — a ledger entry with zero hits is stale.
    let mut ledger_hits: HashMap<&str, usize> =
        OUT_OF_SCOPE_LEDGER.iter().map(|(m, _)| (*m, 0)).collect();

    for f in &files {
        let src =
            std::fs::read_to_string(f).unwrap_or_else(|e| panic!("read {}: {e}", f.display()));
        let lines: Vec<&str> = src.lines().collect();
        let ast = syn::parse_file(&src).unwrap_or_else(|e| panic!("parse {}: {e}", f.display()));

        let mut structs = Vec::new();
        flatten_structs(&ast.items, &mut structs);

        let guarded = under_any(f, &root, GUARDED_SCOPE);

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
                if guarded {
                    guarded_option_fields_seen += 1;
                }
                if serde_handles_absence(&field.attrs) {
                    continue;
                }
                let ident = field.ident.as_ref().expect("named field has an identifier");
                let line = ident.span().start().line;
                if has_always_emit_marker(&lines, line) {
                    continue;
                }
                let rel = f.strip_prefix(&root).unwrap_or(f).display();
                let entry = format!(
                    "  {}: {}::{} (Option field @ line {})",
                    rel, s.ident, ident, line
                );
                if guarded {
                    guarded_violations.push(entry);
                } else if let Some(m) = ledger_module_of(f, &root) {
                    *ledger_hits.get_mut(m).expect("ledger module pre-seeded") += 1;
                } else {
                    unclassified_violations.push(entry);
                }
            }
        }
    }

    // Anti-vacuous floors: if detection breaks, these collapse.
    assert!(
        option_fields_seen >= MIN_OPTION_FIELDS_SEEN,
        "JSON-noise guard saw only {} Option fields across src/ (floor {}); \
         struct/field detection likely broke and the guard would pass vacuously",
        option_fields_seen,
        MIN_OPTION_FIELDS_SEEN,
    );
    assert!(
        guarded_option_fields_seen >= MIN_GUARDED_OPTION_FIELDS,
        "JSON-noise guard saw only {} Option fields in the GUARDED set (floor \
         {}); guarded-struct detection likely broke",
        guarded_option_fields_seen,
        MIN_GUARDED_OPTION_FIELDS,
    );

    // Anti-stale ledger: an entry that no longer matches any unhandled Option is
    // dead weight that quietly widens the escape hatch — force its removal.
    let stale_ledger: Vec<&str> = OUT_OF_SCOPE_LEDGER
        .iter()
        .map(|(m, _)| *m)
        .filter(|m| ledger_hits.get(m).copied().unwrap_or(0) == 0)
        .collect();
    assert!(
        stale_ledger.is_empty(),
        "JSON-noise guard: OUT_OF_SCOPE_LEDGER has stale entries with no \
         matching unhandled Option (module handled its Options, moved, or was \
         renamed). Remove them so the escape hatch stays tight:\n{}",
        stale_ledger.join("\n"),
    );

    // Completeness tripwire: a new payload module cannot re-decay the surface
    // invisibly.
    assert!(
        unclassified_violations.is_empty(),
        "JSON-noise completeness tripwire: {} Option field(s) on a Serialize \
         struct live in a module that is neither GUARDED nor in \
         OUT_OF_SCOPE_LEDGER.\n\n\
         A new (or newly-noisy) serialized module escaped coverage. For each, \
         either:\n\
         \x20 - add its module to GUARDED_SCOPE and annotate the field \
         (skip-when-none / // always-emit), if it reaches an agent, OR\n\
         \x20 - add its module to OUT_OF_SCOPE_LEDGER with a rationale, if its \
         null is acceptable under a different contract.\n\n\
         Unclassified:\n{}",
        unclassified_violations.len(),
        unclassified_violations.join("\n"),
    );

    // Primary guard: unhandled Option in the GUARDED set.
    assert!(
        guarded_violations.is_empty(),
        "JSON-noise re-decay guard: {} Option field(s) on a GUARDED \
         JSON-emitting path neither skip-when-none nor carry an explicit \
         always-emit marker.\n\n\
         Each field below emits `\"<name>\": null` when None, rebuilding the \
         noise the SNR audit removed. Fix each by adding either:\n\
         \x20 #[serde(skip_serializing_if = \"Option::is_none\")]   (if null carries no info)\n\
         \x20 // always-emit: <reason>                              (if null is load-bearing)\n\n\
         Violations:\n{}",
        guarded_violations.len(),
        guarded_violations.join("\n"),
    );
}
