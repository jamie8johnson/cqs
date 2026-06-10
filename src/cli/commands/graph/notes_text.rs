//! Shared agent-facing strings for graph-command kind-mismatch fallbacks.
//!
//! Every graph command (callers, callees, deps, test-map, trace, impact)
//! emits a kind-labeled fallback when the queried name resolves to a kind
//! the command can't process (a const has no callers, a type has no
//! call-graph impact, …). Each fallback carries three strings:
//!
//! - `note` — the structured (`--json`) redirect, surfaced as the
//!   `note` field of the fallback object. Longer; explains *why* the
//!   command doesn't apply and *where* to go instead.
//! - `text_lead` — the first line of the plain-text rendering
//!   (`(impact) `Foo` is a type, not a function — …`).
//! - `text_redirect` — the trailing redirect line of the plain-text
//!   rendering.
//!
//! Before this module these three strings existed as ~24 near-duplicate
//! literals split across the six CLI commands and the daemon's
//! `KindNotes` blocks; the two surfaces drifted independently. Collapsing
//! them here makes the CLI adapter and the daemon adapter reference the
//! same text — one edit site per message.
//!
//! `text_lead` is templated on the queried `name` (and, for trace, the
//! `source` name), so it is produced by a small formatter rather than a
//! bare const. `note` and `text_redirect` are name-independent consts.

/// The four kinds that trigger a graph-command fallback. `Multiple`,
/// `Other`, `Function`, and `NotFound` route through the command's normal
/// flow and never reach this table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FallbackKind {
    Const,
    Type,
    Module,
    Ambiguous,
}

impl FallbackKind {
    /// The lowercase label emitted as the `kind` field of the fallback
    /// object (`"const"`, `"type"`, `"module"`, `"ambiguous"`).
    pub(crate) fn label(self) -> &'static str {
        match self {
            FallbackKind::Const => "const",
            FallbackKind::Type => "type",
            FallbackKind::Module => "module",
            FallbackKind::Ambiguous => "ambiguous",
        }
    }
}

/// Resolved fallback text for one (command, kind) cell. `note` and
/// `text_redirect` are static; `text_lead` is rendered separately because
/// it interpolates the queried name.
pub(crate) struct FallbackText {
    /// `--json` redirect string (the fallback object's `note` field).
    pub note: &'static str,
    /// Trailing redirect line of the plain-text rendering.
    pub text_redirect: &'static str,
}

// ─── callers ────────────────────────────────────────────────────────────────

const CALLERS_CONST_NOTE: &str = "consts don't have callers; here are the definition sites. \
     Use `cqs <name>` or `cqs search <name>` to find references.";
const CALLERS_CONST_REDIRECT: &str = "Use `cqs <name>` or `cqs search <name>` to find references.";
const CALLERS_TYPE_NOTE: &str =
    "types don't have callers in the call-graph sense; here are the definition sites. \
     Use `cqs deps <name>` for type-dependency callers or `cqs <name>` to find usage references.";
const CALLERS_TYPE_REDIRECT: &str =
    "Use `cqs deps <name>` for type-dependency analysis or `cqs <name>` to find usage references.";
const CALLERS_MODULE_NOTE: &str =
    "modules don't have callers in the call-graph sense; here are the declaration sites. \
     Use `cqs <name>` to find files that reference this module.";
const CALLERS_MODULE_REDIRECT: &str = "Use `cqs <name>` to find files that reference this module.";
const CALLERS_AMBIGUOUS_NOTE: &str = "name resolves across multiple kinds (function/type/const/etc.); here are all matches. \
     Re-run `cqs callers <name>` against a more specific name (e.g. `Type::method`) or use `cqs <name>` to disambiguate.";
const CALLERS_AMBIGUOUS_REDIRECT: &str =
    "Re-run with a more specific name (e.g. `Type::method`) or use `cqs <name>` to disambiguate.";

/// Fallback text for `cqs callers <name>`.
pub(crate) fn callers(kind: FallbackKind) -> FallbackText {
    match kind {
        FallbackKind::Const => FallbackText {
            note: CALLERS_CONST_NOTE,
            text_redirect: CALLERS_CONST_REDIRECT,
        },
        FallbackKind::Type => FallbackText {
            note: CALLERS_TYPE_NOTE,
            text_redirect: CALLERS_TYPE_REDIRECT,
        },
        FallbackKind::Module => FallbackText {
            note: CALLERS_MODULE_NOTE,
            text_redirect: CALLERS_MODULE_REDIRECT,
        },
        FallbackKind::Ambiguous => FallbackText {
            note: CALLERS_AMBIGUOUS_NOTE,
            text_redirect: CALLERS_AMBIGUOUS_REDIRECT,
        },
    }
}

/// Plain-text lead line for a `cqs callers <name>` fallback.
pub(crate) fn callers_lead(kind: FallbackKind, name: &str) -> String {
    match kind {
        FallbackKind::Const => format!(
            "(callers) `{name}` is a const, not a function — call-graph callers analysis doesn't apply."
        ),
        FallbackKind::Type => format!(
            "(callers) `{name}` is a type, not a function — call-graph callers analysis doesn't apply."
        ),
        FallbackKind::Module => format!(
            "(callers) `{name}` is a module/namespace, not a function — call-graph callers analysis doesn't apply."
        ),
        FallbackKind::Ambiguous => {
            format!("(callers) `{name}` is ambiguous — matches multiple chunk kinds.")
        }
    }
}

// ─── callees ────────────────────────────────────────────────────────────────

const CALLEES_CONST_NOTE: &str = "consts don't have callees; the const's value is its content. \
     Use `cqs explain <name>` or `cqs read --focus <name>` to inspect.";
const CALLEES_CONST_REDIRECT: &str =
    "Use `cqs explain <name>` or `cqs read --focus <name>` to inspect the value.";
const CALLEES_TYPE_NOTE: &str = "types don't have callees; here are the definition sites. \
     Use `cqs deps <name>` for the type's type dependencies or `cqs callees <Type::method>` for a specific method's callees.";
const CALLEES_TYPE_REDIRECT: &str = "Use `cqs deps <name>` for type-dependency analysis or call against a specific method (`Type::method`).";
const CALLEES_MODULE_NOTE: &str = "modules don't have callees; here are the declaration sites. \
     Use `cqs callees <function-in-module>` for a specific function's callees.";
const CALLEES_MODULE_REDIRECT: &str =
    "Use `cqs callees <function-in-module>` for a specific function's callees.";
const CALLEES_AMBIGUOUS_NOTE: &str =
    "name resolves across multiple kinds (function/type/const/etc.); here are all matches. \
     Re-run `cqs callees <name>` against a more specific name (e.g. `Type::method`).";
const CALLEES_AMBIGUOUS_REDIRECT: &str = "Re-run with a more specific name (e.g. `Type::method`).";

/// Fallback text for `cqs callees <name>`.
pub(crate) fn callees(kind: FallbackKind) -> FallbackText {
    match kind {
        FallbackKind::Const => FallbackText {
            note: CALLEES_CONST_NOTE,
            text_redirect: CALLEES_CONST_REDIRECT,
        },
        FallbackKind::Type => FallbackText {
            note: CALLEES_TYPE_NOTE,
            text_redirect: CALLEES_TYPE_REDIRECT,
        },
        FallbackKind::Module => FallbackText {
            note: CALLEES_MODULE_NOTE,
            text_redirect: CALLEES_MODULE_REDIRECT,
        },
        FallbackKind::Ambiguous => FallbackText {
            note: CALLEES_AMBIGUOUS_NOTE,
            text_redirect: CALLEES_AMBIGUOUS_REDIRECT,
        },
    }
}

/// Plain-text lead line for a `cqs callees <name>` fallback.
pub(crate) fn callees_lead(kind: FallbackKind, name: &str) -> String {
    match kind {
        FallbackKind::Const => format!(
            "(callees) `{name}` is a const, not a function — call-graph callees analysis doesn't apply."
        ),
        FallbackKind::Type => format!(
            "(callees) `{name}` is a type, not a function — call-graph callees analysis doesn't apply."
        ),
        FallbackKind::Module => format!(
            "(callees) `{name}` is a module/namespace, not a function — call-graph callees analysis doesn't apply."
        ),
        FallbackKind::Ambiguous => {
            format!("(callees) `{name}` is ambiguous — matches multiple chunk kinds.")
        }
    }
}

// ─── deps ───────────────────────────────────────────────────────────────────
//
// deps is dual-mode (forward = "type users", reverse = "function's used
// types"), so Function and Type both have valid semantics and never fall
// back. Only Const / Module / Ambiguous reach this table.

const DEPS_CONST_NOTE: &str =
    "consts don't have type dependencies in either direction; here are the definition sites. \
     Use `cqs <name>` to find references to this const.";
const DEPS_CONST_REDIRECT: &str = "Use `cqs <name>` to find references to this const.";
const DEPS_MODULE_NOTE: &str =
    "modules don't have type dependencies in this view; here are the declaration sites. \
     Use `cqs deps <type-or-function-in-module>` for an item-level analysis.";
const DEPS_MODULE_REDIRECT: &str =
    "Use `cqs deps <type-or-function-in-module>` for an item-level analysis.";
const DEPS_AMBIGUOUS_NOTE: &str =
    "name resolves across multiple kinds (function/type/const/etc.); here are all matches. \
     Re-run `cqs deps <name>` against a more specific name (e.g. `Type::method`).";
const DEPS_AMBIGUOUS_REDIRECT: &str = "Re-run with a more specific name (e.g. `Type::method`).";

/// Fallback text for `cqs deps <name>`. `Type` never reaches this — deps
/// runs the forward query for a type — so this returns `None` for Type.
pub(crate) fn deps(kind: FallbackKind) -> Option<FallbackText> {
    match kind {
        FallbackKind::Const => Some(FallbackText {
            note: DEPS_CONST_NOTE,
            text_redirect: DEPS_CONST_REDIRECT,
        }),
        FallbackKind::Module => Some(FallbackText {
            note: DEPS_MODULE_NOTE,
            text_redirect: DEPS_MODULE_REDIRECT,
        }),
        FallbackKind::Ambiguous => Some(FallbackText {
            note: DEPS_AMBIGUOUS_NOTE,
            text_redirect: DEPS_AMBIGUOUS_REDIRECT,
        }),
        FallbackKind::Type => None,
    }
}

/// Plain-text lead line for a `cqs deps <name>` fallback. Returns `None`
/// for Type for the same reason as [`deps`].
pub(crate) fn deps_lead(kind: FallbackKind, name: &str) -> Option<String> {
    match kind {
        FallbackKind::Const => Some(format!(
            "(deps) `{name}` is a const, not a function or type — type-dependency analysis doesn't apply."
        )),
        FallbackKind::Module => Some(format!(
            "(deps) `{name}` is a module/namespace, not a function or type — type-dependency analysis doesn't apply at this granularity."
        )),
        FallbackKind::Ambiguous => {
            Some(format!("(deps) `{name}` is ambiguous — matches multiple chunk kinds."))
        }
        FallbackKind::Type => None,
    }
}

// ─── test-map ───────────────────────────────────────────────────────────────

const TEST_MAP_CONST_NOTE: &str = "consts don't have a call-graph; tests don't 'cover' a const value the way they cover a function. \
     Use `cqs <name>` to find tests that reference this const by name.";
const TEST_MAP_CONST_REDIRECT: &str =
    "Use `cqs <name>` to find tests that reference this const by name.";
const TEST_MAP_TYPE_NOTE: &str = "types don't have a call-graph in the same sense; here are the type's definition sites. \
     Use `cqs <name>` to find tests that reference this type, or `cqs test-map <Type::method>` for a specific method's coverage.";
const TEST_MAP_TYPE_REDIRECT: &str =
    "Use `cqs <name>` to find tests that reference this type or call against a specific method.";
const TEST_MAP_MODULE_NOTE: &str = "modules don't have a call-graph; tests cover specific functions inside the module, not the module itself. \
     Use `cqs <name>` to find tests in this module's files.";
const TEST_MAP_MODULE_REDIRECT: &str = "Use `cqs <name>` to find tests in the module's files.";
const TEST_MAP_AMBIGUOUS_NOTE: &str =
    "name resolves across multiple kinds (function/type/const/etc.); here are all matches. \
     Re-run `cqs test-map <name>` against a more specific name (e.g. `Type::method`).";
const TEST_MAP_AMBIGUOUS_REDIRECT: &str = "Re-run with a more specific name (e.g. `Type::method`).";

/// Fallback text for `cqs test-map <name>`.
pub(crate) fn test_map(kind: FallbackKind) -> FallbackText {
    match kind {
        FallbackKind::Const => FallbackText {
            note: TEST_MAP_CONST_NOTE,
            text_redirect: TEST_MAP_CONST_REDIRECT,
        },
        FallbackKind::Type => FallbackText {
            note: TEST_MAP_TYPE_NOTE,
            text_redirect: TEST_MAP_TYPE_REDIRECT,
        },
        FallbackKind::Module => FallbackText {
            note: TEST_MAP_MODULE_NOTE,
            text_redirect: TEST_MAP_MODULE_REDIRECT,
        },
        FallbackKind::Ambiguous => FallbackText {
            note: TEST_MAP_AMBIGUOUS_NOTE,
            text_redirect: TEST_MAP_AMBIGUOUS_REDIRECT,
        },
    }
}

/// Plain-text lead line for a `cqs test-map <name>` fallback.
pub(crate) fn test_map_lead(kind: FallbackKind, name: &str) -> String {
    match kind {
        FallbackKind::Const => format!(
            "(test-map) `{name}` is a const, not a function — call-graph test-map analysis doesn't apply."
        ),
        FallbackKind::Type => format!(
            "(test-map) `{name}` is a type, not a function — call-graph test-map analysis doesn't apply."
        ),
        FallbackKind::Module => format!(
            "(test-map) `{name}` is a module/namespace, not a function — call-graph test-map analysis doesn't apply."
        ),
        FallbackKind::Ambiguous => {
            format!("(test-map) `{name}` is ambiguous — matches multiple chunk kinds.")
        }
    }
}

// ─── trace ──────────────────────────────────────────────────────────────────
//
// trace classifies the *source* name; the redirect copy talks about
// "source" rather than the bare name.

const TRACE_CONST_NOTE: &str =
    "consts don't participate in the call-graph; no call path can originate from a const value. \
     Use `cqs <source>` to find references and trace from the calling functions.";
const TRACE_CONST_REDIRECT: &str =
    "Use `cqs <source>` to find references and trace from the calling functions.";
const TRACE_TYPE_NOTE: &str = "types don't have call chains; here are the type's definition sites. \
     Use `cqs <source>` to find usage references or `cqs trace <method-on-type> <target>` for a specific method.";
const TRACE_TYPE_REDIRECT: &str =
    "Use `cqs <source>` to find usage references or trace from a specific method.";
const TRACE_MODULE_NOTE: &str = "modules don't participate in the call-graph as nodes. \
     Use `cqs <source>` to find files that reference this module, or `cqs trace <function-in-module> <target>`.";
const TRACE_MODULE_REDIRECT: &str =
    "Use `cqs trace <function-in-module> <target>` for a specific function.";
const TRACE_AMBIGUOUS_NOTE: &str =
    "source name resolves across multiple kinds (function/type/const/etc.); here are all matches. \
     Re-run `cqs trace <source> <target>` against a more specific name.";
const TRACE_AMBIGUOUS_REDIRECT: &str = "Re-run with a more specific name (e.g. `Type::method`).";

/// Fallback text for `cqs trace <source> <target>` (classified on source).
pub(crate) fn trace(kind: FallbackKind) -> FallbackText {
    match kind {
        FallbackKind::Const => FallbackText {
            note: TRACE_CONST_NOTE,
            text_redirect: TRACE_CONST_REDIRECT,
        },
        FallbackKind::Type => FallbackText {
            note: TRACE_TYPE_NOTE,
            text_redirect: TRACE_TYPE_REDIRECT,
        },
        FallbackKind::Module => FallbackText {
            note: TRACE_MODULE_NOTE,
            text_redirect: TRACE_MODULE_REDIRECT,
        },
        FallbackKind::Ambiguous => FallbackText {
            note: TRACE_AMBIGUOUS_NOTE,
            text_redirect: TRACE_AMBIGUOUS_REDIRECT,
        },
    }
}

/// Plain-text lead line for a `cqs trace <source> <target>` fallback.
pub(crate) fn trace_lead(kind: FallbackKind, source: &str) -> String {
    match kind {
        FallbackKind::Const => {
            format!("(trace) source `{source}` is a const, not a function — no call path applies.")
        }
        FallbackKind::Type => {
            format!("(trace) source `{source}` is a type, not a function — no call path applies.")
        }
        FallbackKind::Module => format!(
            "(trace) source `{source}` is a module/namespace, not a function — no call path applies."
        ),
        FallbackKind::Ambiguous => {
            format!("(trace) source `{source}` is ambiguous — matches multiple chunk kinds.")
        }
    }
}

// ─── impact ─────────────────────────────────────────────────────────────────

const IMPACT_CONST_NOTE: &str =
    "consts don't have call-graph impact; here are the definition sites. \
     Use `cqs <name>` or `cqs search <name>` to find references.";
const IMPACT_CONST_REDIRECT: &str = "Use `cqs <name>` or `cqs search <name>` to find references.";
const IMPACT_TYPE_NOTE: &str =
    "types don't have call-graph impact; here are the definition sites. \
     Use `cqs deps <name>` for type-dependency analysis or `cqs <name>` to find usage references.";
const IMPACT_TYPE_REDIRECT: &str =
    "Use `cqs deps <name>` for type-dependency analysis or `cqs <name>` to find usage references.";
const IMPACT_MODULE_NOTE: &str =
    "modules don't have call-graph impact; here are the declaration sites. \
     Use `cqs <name>` to find files that reference this module.";
const IMPACT_MODULE_REDIRECT: &str = "Use `cqs <name>` to find files that reference this module.";
const IMPACT_AMBIGUOUS_NOTE: &str = "name resolves across multiple kinds (function/type/const/etc.); here are all matches. \
     Re-run `cqs impact <name>` against a more specific name (e.g. `Type::method`) or use `cqs <name>` to disambiguate by content.";
const IMPACT_AMBIGUOUS_REDIRECT: &str =
    "Re-run with a more specific name (e.g. `Type::method`) or use `cqs <name>` to disambiguate.";

/// Fallback text for `cqs impact <name>`.
pub(crate) fn impact(kind: FallbackKind) -> FallbackText {
    match kind {
        FallbackKind::Const => FallbackText {
            note: IMPACT_CONST_NOTE,
            text_redirect: IMPACT_CONST_REDIRECT,
        },
        FallbackKind::Type => FallbackText {
            note: IMPACT_TYPE_NOTE,
            text_redirect: IMPACT_TYPE_REDIRECT,
        },
        FallbackKind::Module => FallbackText {
            note: IMPACT_MODULE_NOTE,
            text_redirect: IMPACT_MODULE_REDIRECT,
        },
        FallbackKind::Ambiguous => FallbackText {
            note: IMPACT_AMBIGUOUS_NOTE,
            text_redirect: IMPACT_AMBIGUOUS_REDIRECT,
        },
    }
}

/// Plain-text lead line for a `cqs impact <name>` fallback.
pub(crate) fn impact_lead(kind: FallbackKind, name: &str) -> String {
    match kind {
        FallbackKind::Const => format!(
            "(impact) `{name}` is a const, not a function — call-graph impact analysis doesn't apply."
        ),
        FallbackKind::Type => format!(
            "(impact) `{name}` is a type, not a function — call-graph impact analysis doesn't apply."
        ),
        FallbackKind::Module => format!(
            "(impact) `{name}` is a module/namespace, not a function — call-graph impact analysis doesn't apply."
        ),
        FallbackKind::Ambiguous => {
            format!("(impact) `{name}` is ambiguous — matches multiple chunk kinds.")
        }
    }
}
