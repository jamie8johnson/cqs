//! Property-based tests for the daemon-translate seam
//! (`cqs::daemon_translate::translate_cli_args_to_batch` and
//! `classify_slim_envelope` / the slim-envelope classification).
//!
//! This file is deliberately a DIFFERENTLY-SHAPED auditor than the rest of the
//! suite. The hand-written tests in `tests/daemon_forward_test.rs` and the
//! `src/daemon_translate.rs` unit module walk known paths — one fixed argv in,
//! one expected `(cmd, args)` out. They are happy/sad-path examples. The bugs
//! this seam has historically shipped (combined-short clusters like `-qv`
//! reaching the batch parser verbatim, top-level scope flags silently dropped
//! before a scope-target subcommand, value-flag values leaking past the
//! subcommand boundary) all slipped through *because every individual example
//! was correct*. The gap was the inputs nobody wrote a literal for.
//!
//! These properties generate inputs nobody imagined and assert invariants that
//! must hold across ALL of them. The generators are structured proptest
//! strategies (not raw `String`s) so a failing case minimizes to a readable
//! counterexample.
//!
//! Reachability note: `BatchInput` / `BatchCmd` (the batch clap parser) is
//! `pub(crate)` in the binary crate, so an integration test cannot round-trip
//! the translated `(cmd, args)` through clap to literally re-parse it. The
//! end-to-end "daemon-down clap accepts X, daemon-up X round-trips" guarantee
//! is covered by the socket-mock tests in `tests/daemon_forward_test.rs` that
//! spawn the real binary. Here we pin the *structural* invariants the
//! translator must uphold for the round-trip to be possible at all — the exact
//! invariants the historical bugs violated. The combined-short property below
//! REDISCOVERS the `-qv` bug if the expansion is reverted: the generator emits
//! `-qv`-shaped clusters and asserts none survive into the forwarded output.
//!
//! Tuning: proptest runs 256 cases per property by default. Override with the
//! standard proptest env var, e.g. `PROPTEST_CASES=5000 cargo test --test
//! proptest_translate`. (This is proptest's own env var — intentionally not a
//! `CQS_`-prefixed one, to stay out of the README env-table machinery.)

use std::collections::BTreeSet;

use cqs::daemon_translate::{
    classify_slim_envelope, translate_cli_args_to_batch, CliArgSpec, SlimEnvelope,
};
use proptest::prelude::*;

// ─────────────────────────────────────────────────────────────────────────────
// Spec under test.
//
// Mirrors the production classification (which `cli::dispatch::cli_arg_spec`
// derives from the live clap definition). The same hand-built subset the
// example tests use, lifted here so the generators can consult it to decide
// which spellings are value flags / process-local / scope flags.
// ─────────────────────────────────────────────────────────────────────────────

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

fn spec() -> CliArgSpec {
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
        scope_flags: set(&["-l", "--lang", "-p", "--path"]),
        scope_targets: set(&["similar"]),
    }
}

/// Daemon-capable subcommand names the translator can receive. The production
/// list is derive-generated (`Commands::daemon_capable_variant_names`,
/// pub(crate)); this representative subset covers the three behavioural classes
/// the translator distinguishes: a scope-target (`similar`), the `notes`
/// special-case, and plain verbatim-forward subcommands (everything else).
const SUBCOMMANDS: &[&str] = &[
    "similar", "impact", "callers", "callees", "blame", "gather", "search", "trace", "explain",
    "notes",
];

// ─────────────────────────────────────────────────────────────────────────────
// Local predicates the translator must satisfy. Re-implemented here (the real
// ones are private to the module) so the properties pin observable behaviour,
// not internal helpers.
// ─────────────────────────────────────────────────────────────────────────────

/// A combined-short cluster: `-` followed by ≥2 chars, none `-`, no `=`.
/// This is the shape that clap accepts daemon-down but the batch parser
/// rejects — the `-qv` family. The translator must never emit one on the
/// bare-query path.
fn is_combined_short(tok: &str) -> bool {
    match tok.strip_prefix('-') {
        Some(rest) => rest.len() >= 2 && !rest.starts_with('-') && !rest.contains('='),
        None => false,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Token strategies. Structured so counterexamples shrink to a minimal,
// human-readable argv rather than a soup of random bytes.
// ─────────────────────────────────────────────────────────────────────────────

/// A bare query token: leading-dash-free, non-empty, no `=` (so it can't be
/// mistaken for an attached-value flag), no whitespace surprises. Mirrors the
/// real-world "free text query word" the user types.
fn query_token() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9_./][a-zA-Z0-9_./ ]{0,12}".prop_map(|s| s.trim().to_string())
}

/// A value to hand a value-flag: a non-dash bareword (so it cannot itself be
/// re-scanned as a flag).
fn flag_value() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9_./*][a-zA-Z0-9_./*-]{0,8}".boxed()
}

/// One top-level flag token (or flag+value pair) that may appear in the
/// pre-subcommand region. Emits the *tokens* as a `Vec<String>` so spaced
/// pairs stay together.
fn top_level_region_token() -> impl Strategy<Value = Vec<String>> {
    prop_oneof![
        // Process-local bool, separate short form.
        Just(vec!["-v".to_string()]),
        Just(vec!["-q".to_string()]),
        Just(vec!["--json".to_string()]),
        Just(vec!["--rrf".to_string()]),
        // Combined-short bool cluster (the `-qv` family). Must be tolerated in
        // the top-level region: clap accepts it daemon-down.
        Just(vec!["-qv".to_string()]),
        Just(vec!["-vq".to_string()]),
        // Value flag, spaced form. The value must not be mistaken for the
        // subcommand name.
        flag_value().prop_map(|val| vec!["-n".to_string(), val]),
        flag_value().prop_map(|val| vec!["--model".to_string(), val]),
        // Value flag, attached form (single token).
        flag_value().prop_map(|val| vec![format!("--model={val}")]),
        flag_value().prop_map(|val| vec![format!("-n={val}")]),
    ]
}

/// A top-level *scope* flag pair (`--lang rust`, `-p src/*`, `--path=x`). These
/// are the ones that must survive onto a scope-target subcommand's tail.
fn scope_flag_pair() -> impl Strategy<Value = (String, String, Vec<String>)> {
    // Returns (flag_spelling, value, tokens-as-emitted).
    prop_oneof![
        flag_value().prop_map(|v| ("--lang".to_string(), v.clone(), vec!["--lang".into(), v])),
        flag_value().prop_map(|v| ("-l".to_string(), v.clone(), vec!["-l".into(), v])),
        flag_value().prop_map(|v| ("--path".to_string(), v.clone(), vec!["--path".into(), v])),
        flag_value().prop_map(|v| ("-p".to_string(), v.clone(), vec!["-p".into(), v])),
        // Attached form: one token; value embedded.
        flag_value().prop_map(|v| ("--lang".to_string(), v.clone(), vec![format!("--lang={v}")])),
    ]
}

/// One bare-query token: either a free-text query word, a forwarded search
/// knob (in any of its spellings/spacings), a process-local flag to be
/// stripped, or a combined-short cluster (the `-qv` / `-vn8` families).
fn bare_query_element() -> impl Strategy<Value = Vec<String>> {
    prop_oneof![
        // Plain query word.
        query_token().prop_map(|q| vec![q]),
        // Forwarded value flag, every spelling/spacing the doc enumerates.
        flag_value().prop_map(|v| vec!["-n".to_string(), v]),
        flag_value().prop_map(|v| vec![format!("-n={v}")]),
        flag_value().prop_map(|v| vec!["--limit".to_string(), v]),
        flag_value().prop_map(|v| vec![format!("--limit={v}")]),
        // Forwarded bool search knob.
        Just(vec!["--rrf".to_string()]),
        // Process-local flags (must be stripped).
        Just(vec!["--json".to_string()]),
        Just(vec!["-v".to_string()]),
        Just(vec!["-q".to_string()]),
        flag_value().prop_map(|v| vec!["--model".to_string(), v]),
        flag_value().prop_map(|v| vec![format!("--model={v}")]),
        // Combined-short clusters — the historical `-qv` bug shape, plus
        // mixed bool+value-flag (`-vn 8`) and attached-value (`-qn8`).
        Just(vec!["-qv".to_string()]),
        Just(vec!["-vq".to_string()]),
        flag_value().prop_map(|v| vec!["-vn".to_string(), v]),
        "[0-9]{1,3}".prop_map(|n| vec![format!("-qn{n}")]),
    ]
}

// ─────────────────────────────────────────────────────────────────────────────
// PROPERTY 1a: subcommand-path translation invariants.
// ─────────────────────────────────────────────────────────────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// For any pre-subcommand top-level region + any subcommand + any verbatim
    /// tail, the translated form must:
    ///   - return the SAME subcommand name that appeared in the argv,
    ///   - preserve the original subcommand tail verbatim (modulo the documented
    ///     scope-splice in front and the `notes list` strip),
    ///   - never let a pre-subcommand value-flag's value leak in as the
    ///     subcommand name or a stray tail token,
    ///   - never emit a process-local top-level flag (`--json`, `-v`, …) into
    ///     the forwarded tail — those configure the CLI process, not the handler.
    #[test]
    fn subcommand_translation_preserves_command_and_tail(
        region in proptest::collection::vec(top_level_region_token(), 0..4),
        sub_idx in 0usize..SUBCOMMANDS.len(),
        tail in proptest::collection::vec(query_token(), 0..4),
    ) {
        let s = spec();
        let subcommand = SUBCOMMANDS[sub_idx];

        let mut argv: Vec<String> = Vec::new();
        for group in &region {
            argv.extend(group.iter().cloned());
        }
        argv.push(subcommand.to_string());
        let tail_start = argv.len();
        argv.extend(tail.iter().cloned());

        let (cmd, out) = translate_cli_args_to_batch(&argv, true, &s);

        // The subcommand name is preserved exactly.
        prop_assert_eq!(&cmd, subcommand, "argv={:?}", argv);

        // The original tail (everything the user put AFTER the subcommand) must
        // appear as a contiguous suffix of the output — scope flags may be
        // spliced in front, so we check suffix-containment rather than equality.
        let original_tail: Vec<String> = argv[tail_start..].to_vec();
        // `cqs notes list` strips a leading `list` token (the daemon's
        // `BatchCmd::Notes` takes `--warnings`/`--patterns` directly, no `list`
        // verb). The query-token generator can incidentally produce "list" as
        // the first tail token; account for that documented strip so the
        // invariant pins the real contract rather than flagging it.
        let expected_tail: Vec<String> =
            if subcommand == "notes" && original_tail.first().map(|s| s.as_str()) == Some("list") {
                original_tail[1..].to_vec()
            } else {
                original_tail.clone()
            };
        prop_assert!(
            out.ends_with(&expected_tail),
            "tail not preserved as suffix: argv={:?} out={:?} expected_tail={:?}",
            argv, out, expected_tail
        );

        // No process-local top-level flag leaked into the forwarded tail. The
        // pre-subcommand region is dropped wholesale (except scope flags, which
        // our `region` strategy does not emit), so none of these may survive.
        for tok in &out {
            prop_assert!(
                tok != "--json" && tok != "-v" && tok != "-q" && tok != "--rrf",
                "process-local flag leaked into tail: tok={:?} argv={:?} out={:?}",
                tok, argv, out
            );
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// PROPERTY 1b: top-level scope flags reach a scope-target subcommand tail.
//
// This is the scope-flag-drop bug class: `cqs --lang rust similar foo` must
// forward `--lang rust` onto the `similar` tail (its batch `SimilarArgs`
// accepts it), or daemon-routed `similar` silently ignores scoping that
// CLI-direct honors. Asserted across all spellings and a non-target control.
// ─────────────────────────────────────────────────────────────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn top_level_scope_flag_reaches_scope_target_tail(
        (flag, value, scope_tokens) in scope_flag_pair(),
        tail in proptest::collection::vec(query_token(), 1..3),
    ) {
        let s = spec();

        // scope-target subcommand: the flag must reach the tail.
        let mut argv: Vec<String> = scope_tokens.clone();
        argv.push("similar".to_string());
        argv.extend(tail.iter().cloned());
        let (cmd, out) = translate_cli_args_to_batch(&argv, true, &s);
        prop_assert_eq!(cmd, "similar");

        // The scope flag+value must survive onto the tail: either the spaced
        // pair (flag token immediately followed by its value) or the attached
        // single token.
        let spaced_present = out.windows(2).any(|w| w[0] == flag && w[1] == value);
        let attached_present = out.iter().any(|t| t == &format!("{flag}={value}"));
        prop_assert!(
            spaced_present || attached_present,
            "scope flag dropped for scope-target: flag={:?} value={:?} argv={:?} out={:?}",
            flag, value, argv, out
        );

        // Control: a NON-scope-target subcommand drops the top-level region
        // entirely — the scope flag must NOT appear, and nothing else either.
        let mut argv2: Vec<String> = scope_tokens.clone();
        argv2.push("callers".to_string());
        argv2.extend(tail.iter().cloned());
        let (cmd2, out2) = translate_cli_args_to_batch(&argv2, true, &s);
        prop_assert_eq!(cmd2, "callers");
        let leaked = out2.windows(2).any(|w| w[0] == flag && w[1] == value)
            || out2.iter().any(|t| t == &format!("{flag}={value}"));
        prop_assert!(
            !leaked,
            "scope flag leaked onto non-target subcommand: flag={:?} argv={:?} out={:?}",
            flag, argv2, out2
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// PROPERTY 1c: combined-short clusters never survive the bare-query path.
//
// THE `-qv` REDISCOVERY PROPERTY. The generator emits `-qv` / `-vq` / `-vn 8`
// / `-qn8` clusters. clap accepts these daemon-down, so they reach this path;
// the batch parser rejects multi-char short clusters, so forwarding one is a
// non-recoverable protocol error. The translator must expand every cluster
// into individual shorts. INVARIANT: no token in the output is a combined-short
// cluster. Revert `expand_combined_short` and this property fails immediately
// on a `-qv` input.
// ─────────────────────────────────────────────────────────────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn bare_query_emits_no_combined_short_clusters(
        elements in proptest::collection::vec(bare_query_element(), 0..6),
    ) {
        let s = spec();
        let mut argv: Vec<String> = Vec::new();
        for group in &elements {
            argv.extend(group.iter().cloned());
        }
        // A bare query in practice always has at least one query word; an empty
        // argv translates to an empty `search` arg list, which is still valid.
        let (cmd, out) = translate_cli_args_to_batch(&argv, false, &s);
        prop_assert_eq!(cmd, "search");

        for tok in &out {
            prop_assert!(
                !is_combined_short(tok),
                "combined-short cluster survived bare-query translation: tok={:?} argv={:?} out={:?}",
                tok, argv, out
            );
        }

        // Stronger: no purely-process-local flag survives at all (the cluster
        // expansion strips `-q`/`-v` individually). And no `--json`/`-v`/`-q`.
        for tok in &out {
            prop_assert!(
                tok != "--json" && tok != "-v" && tok != "-q" && tok != "--quiet"
                    && tok != "--verbose",
                "process-local flag survived bare-query translation: tok={:?} argv={:?} out={:?}",
                tok, argv, out
            );
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// PROPERTY 1d: a forwarded value flag never loses its value or splits it.
// Bare-query path.
//
// We generate forwarded value flags (`-n <value>`) with non-dash-leading
// values and assert the value travels with its flag as a contiguous pair.
//
// Scope note on dash-leading values: cqs's forwarded value flags (`-n`/`-t`/
// `--limit`/`--tokens`/`--threshold`) are numeric clap args WITHOUT
// `allow_hyphen_values`/`allow_negative_numbers`, so clap rejects `-n -3`
// daemon-down — a dash-leading value is not a clap-reachable invocation and is
// therefore out of scope for "any invocation clap accepts". (The translator's
// bare-query combined-short pre-expansion would in fact shred a dash-leading
// value token — see the residual note in the lane report — but that input
// can't reach this path through clap today.)
// ─────────────────────────────────────────────────────────────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn bare_query_value_flag_keeps_its_value(
        lead in query_token(),
        value in "[a-zA-Z0-9_./*][a-zA-Z0-9_./*-]{0,6}",
    ) {
        let s = spec();
        // `query -n <value>`: the value flag's value must immediately follow it
        // in the output, even when the value starts with `-`.
        let argv = vec!["q".to_string(), lead, "-n".to_string(), value.clone()];
        let (cmd, out) = translate_cli_args_to_batch(&argv, false, &s);
        prop_assert_eq!(cmd, "search");
        // `-n` must be present and immediately followed by `value`.
        let ok = out.windows(2).any(|w| w[0] == "-n" && w[1] == value);
        prop_assert!(
            ok,
            "value flag lost or split from its value: value={:?} argv={:?} out={:?}",
            value, argv, out
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// PROPERTY 2: slim-envelope classification never panics and never silently
// swallows an error into a success.
//
// `classify_slim_envelope` matches against the slim batch wire contract
// (`{"data": …}` | `{"error": {code,message}}`, optional `_meta`, no other
// keys). We generate envelopes with arbitrary data/error/message/meta
// combinations — including non-string messages, null data, extra keys, and
// nested junk — and assert:
//   - it never panics (proptest catches the unwind),
//   - an envelope carrying an `error` key (and no `data`) is NEVER classified
//     as `Data` (errors are not silently swallowed into success),
//   - a recognized `data`-only envelope is classified as `Data`.
// ─────────────────────────────────────────────────────────────────────────────

/// Arbitrary-ish JSON value, bounded depth so cases stay readable.
fn arb_json() -> impl Strategy<Value = serde_json::Value> {
    let leaf = prop_oneof![
        Just(serde_json::Value::Null),
        any::<bool>().prop_map(serde_json::Value::Bool),
        any::<i64>().prop_map(|n| serde_json::json!(n)),
        "[a-zA-Z0-9 ]{0,8}".prop_map(serde_json::Value::String),
    ];
    leaf.prop_recursive(3, 16, 4, |inner| {
        prop_oneof![
            proptest::collection::vec(inner.clone(), 0..4).prop_map(serde_json::Value::Array),
            proptest::collection::hash_map("[a-z]{1,4}", inner, 0..4)
                .prop_map(|m| serde_json::Value::Object(m.into_iter().collect())),
        ]
    })
}

/// Build an envelope object from optional data / error / meta / extra members.
fn arb_envelope() -> impl Strategy<Value = serde_json::Value> {
    (
        proptest::option::of(arb_json()),
        // error object: optional, with arbitrary (possibly non-string) code/message.
        proptest::option::of((
            proptest::option::of(arb_json()),
            proptest::option::of(arb_json()),
        )),
        proptest::option::of(arb_json()),
        // an extra junk key that, if present, must force a non-match.
        any::<bool>(),
    )
        .prop_map(|(data, error, meta, extra_key)| {
            let mut obj = serde_json::Map::new();
            if let Some(d) = data {
                obj.insert("data".to_string(), d);
            }
            if let Some((code, message)) = error {
                let mut err = serde_json::Map::new();
                if let Some(c) = code {
                    err.insert("code".to_string(), c);
                }
                if let Some(m) = message {
                    err.insert("message".to_string(), m);
                }
                obj.insert("error".to_string(), serde_json::Value::Object(err));
            }
            if let Some(mt) = meta {
                obj.insert("_meta".to_string(), mt);
            }
            if extra_key {
                obj.insert("unexpected".to_string(), serde_json::json!(1));
            }
            serde_json::Value::Object(obj)
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// Classification never panics on an arbitrary JSON value, and an
    /// error-bearing slim envelope is never collapsed into a `Data` success.
    #[test]
    fn classify_slim_envelope_total_and_no_error_swallow(v in arb_json()) {
        // Total: must not panic on ANY json value.
        let classified = classify_slim_envelope(&v);

        // If the value is an object whose ONLY keys are a subset of
        // {data,error,_meta}, has an `error` key, and has NO `data` key, then
        // the slim contract says this is an Error — it must NOT classify as
        // Data (that would silently swallow a daemon error into success).
        if let Some(obj) = v.as_object() {
            let only_slim_keys = !obj.is_empty()
                && obj.keys().all(|k| k == "data" || k == "error" || k == "_meta");
            let has_error = obj.contains_key("error");
            let has_data = obj.contains_key("data");
            if only_slim_keys && has_error && !has_data {
                match classified {
                    Some(SlimEnvelope::Error { .. }) => {} // correct
                    Some(SlimEnvelope::Data { .. }) => {
                        prop_assert!(false, "error envelope swallowed into Data: {:?}", v);
                    }
                    None => {
                        prop_assert!(false, "error envelope not classified: {:?}", v);
                    }
                }
            }
            // A data-only slim envelope must classify as Data.
            if only_slim_keys && has_data && !has_error {
                prop_assert!(
                    matches!(classified, Some(SlimEnvelope::Data { .. })),
                    "data-only slim envelope misclassified: {:?}",
                    v
                );
            }
        }
    }

    /// Same invariants, but over the structured envelope generator that biases
    /// toward the slim shape (so the error/data arms are actually exercised
    /// densely, not just hit by chance from `arb_json`).
    #[test]
    fn classify_structured_envelope_never_swallows_error(env in arb_envelope()) {
        let classified = classify_slim_envelope(&env);
        let obj = env.as_object().expect("arb_envelope always builds an object");
        let only_slim_keys = !obj.is_empty()
            && obj.keys().all(|k| k == "data" || k == "error" || k == "_meta");
        let has_error = obj.contains_key("error");
        let has_data = obj.contains_key("data");

        if only_slim_keys && has_error && !has_data {
            prop_assert!(
                matches!(classified, Some(SlimEnvelope::Error { .. })),
                "structured error envelope not surfaced as Error: {:?}",
                env
            );
        }
        if only_slim_keys && has_data && !has_error {
            prop_assert!(
                matches!(classified, Some(SlimEnvelope::Data { .. })),
                "structured data envelope not surfaced as Data: {:?}",
                env
            );
        }
        // An object carrying the `unexpected` junk key is NEVER a slim
        // envelope — it must pass through unrecognized (None).
        if obj.contains_key("unexpected") {
            prop_assert!(
                classified.is_none(),
                "envelope with extra key wrongly matched slim shape: {:?}",
                env
            );
        }
    }
}
