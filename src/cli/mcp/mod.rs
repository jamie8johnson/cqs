//! `cqs mcp` — the stdio ↔ daemon-socket MCP bridge (Phase 1, Lane 2).
//!
//! A thin, GPU-free process that speaks MCP JSON-RPC over stdio to an MCP
//! client (e.g. Claude Code) and forwards each `tools/call` to the warm `cqs`
//! daemon over its unix socket. It RIDES the command cores: it never
//! reimplements a tool, it deserializes the wire `arguments` object and relays
//! it as the Lane 1 / D3-b JSON-args frame
//! (`{"command": "<cmd>", "arguments": {...}}`) the daemon already accepts.
//!
//! ## Why bridge-only (D4a)
//!
//! There is NO in-process fallback. The bridge is the one process that emits
//! MCP JSON-RPC on its own stdout, and the only process-wide stdout suppressor
//! cqs has is the per-call `stdout_gag` fd-1 redirect — which does NOT cover an
//! embedder/ORT/tokenizer load. An in-process core run would leak that load's
//! stdout into the JSON-RPC channel. So the bridge requires a running daemon
//! and fails clean if absent (a JSON-RPC protocol error per call). The bridge
//! itself loads no model, so its stdout is trivially clean — every `tracing`
//! line routes to stderr (configured in `main.rs`).
//!
//! ## Module layout
//!
//! - [`bridge`] — the stdin → parse → route → stdout loop. Owns NDJSON
//!   framing, the JSON-RPC envelope, the method dispatch table, and the
//!   per-`tools/call` daemon-socket round-trip.
//! - [`lifecycle`] — `initialize` (advertise / accept protocol version) and the
//!   `initialized` notification. Holds the protocol-version constant.
//! - [`tools`] — `tools/list` generation (the read-only registry × Phase-0
//!   `schemars` inputSchemas) and `tools/call` dispatch (deserialize-check →
//!   relay → map the daemon envelope into a `CallToolResult`, with the
//!   error-mapping invariant of Blocker #1).

mod bridge;
mod lifecycle;
mod tools;

pub use bridge::serve_stdio;

/// The operator opt-in for the MCP gated-mutation channel (Phase 2a).
///
/// Default OFF. When unset, `tools/list` emits exactly the Phase-1 read tools
/// (zero delta) and the daemon's JSON-args path rejects notes mutations as
/// before. When set to `1`, the `cqs_notes_add`/`cqs_notes_update`/
/// `cqs_notes_remove` tools are advertised AND the daemon accepts their
/// mutating dispatch. The destructive set (`gc`, `slot remove`, …) is withheld
/// by ABSENCE regardless of this flag — no flag re-enables it in Phase 2a.
pub(crate) const MUTATIONS_ENV: &str = "CQS_MCP_ENABLE_MUTATIONS";

/// Whether the MCP gated-mutation channel is enabled ([`MUTATIONS_ENV`] == `1`).
///
/// Read at both layers so the boundary holds end-to-end: the bridge gates the
/// advertised tool surface (`tool_table`), and the daemon gates the actual
/// mutating dispatch (`json_args::build_batch_cmd`). Both read the SAME process
/// env var, so a raw socket client that bypasses `tools/list` still cannot
/// trigger a notes write unless the operator opted the daemon in.
pub(crate) fn mutations_enabled() -> bool {
    std::env::var(MUTATIONS_ENV).as_deref() == Ok("1")
}

#[cfg(test)]
mod tests {
    use super::{tools, MUTATIONS_ENV};

    /// RAII guard that sets [`MUTATIONS_ENV`] for the duration of a test and
    /// restores the prior value (or unset) on drop. Pairs with
    /// `#[serial_test::serial(mcp_mutations_env)]` so concurrent tests don't
    /// race the process-global env var.
    struct MutationsEnvGuard {
        prior: Option<String>,
    }
    impl MutationsEnvGuard {
        fn set(on: bool) -> Self {
            let prior = std::env::var(MUTATIONS_ENV).ok();
            if on {
                std::env::set_var(MUTATIONS_ENV, "1");
            } else {
                std::env::remove_var(MUTATIONS_ENV);
            }
            Self { prior }
        }
    }
    impl Drop for MutationsEnvGuard {
        fn drop(&mut self) {
            match &self.prior {
                Some(v) => std::env::set_var(MUTATIONS_ENV, v),
                None => std::env::remove_var(MUTATIONS_ENV),
            }
        }
    }

    /// The command names that map to the exposed MCP read tools — the daemon's
    /// JSON-args-capable read set MINUS the gated-mutation arms MINUS the
    /// withheld doc/signature-relay set. Returns the bare daemon command names
    /// (the registry comparison key).
    ///
    /// DERIVED, not hand-mirrored: the capable set comes from
    /// `build_batch_cmd`'s single source of truth
    /// ([`crate::cli::batch::JSON_ARGS_CAPABLE_COMMANDS`]), so a Lane-1 command
    /// added there auto-enrolls in this guard rather than silently skipping it.
    /// The const is itself pinned against the match arms by an exhaustiveness
    /// test in `json_args.rs`, closing the incomplete-sweep gap.
    fn expected_read_commands() -> std::collections::BTreeSet<String> {
        // The gated mutators (`notes-*`, `index`) are JSON-args-capable but are
        // NOT read tools — they ride the flag-gated rows and are accounted for
        // separately. Subtract them here so this set is the READ surface only.
        let gated_mutators: std::collections::BTreeSet<&str> =
            ["notes-add", "notes-update", "notes-remove", "index"]
                .into_iter()
                .collect();
        // The withheld set: the destructive mutators that are withheld by
        // absence — naming them here makes the guard ENFORCE the withhold (a
        // future hand that adds `cqs_gc` to the table fails this test). The
        // doc/signature relay surfaces `context` and `explain` are NO LONGER
        // withheld — their relay is fully injection-scanned (the context.rs /
        // explain.rs RT-RELAY scan==relayed guards pin per-chunk completeness in
        // both compact and full modes), so they are exposed read tools.
        let withheld: std::collections::BTreeSet<&str> = [
            "gc",
            "slot-remove",
            "index-force",
            "model-swap",
            "cache-clear",
        ]
        .into_iter()
        .collect();
        crate::cli::batch::JSON_ARGS_CAPABLE_COMMANDS
            .iter()
            .copied()
            .filter(|c| !gated_mutators.contains(c) && !withheld.contains(c))
            .map(|s| s.to_string())
            .collect()
    }

    /// Map the exposed tool table to its bare daemon command names.
    fn exposed_commands() -> std::collections::BTreeSet<String> {
        tools::tool_table()
            .iter()
            .map(|t| {
                // The MCP tool name carries the `cqs_` prefix (D1) and uses
                // underscores; map it back to the bare daemon command name for
                // the registry comparison. `test-map` is the lone hyphenated
                // command — its MCP name is `cqs_test_map`.
                tools::mcp_name_to_command(t.name)
            })
            .collect()
    }

    /// `tools/list`-matches-registry guard (flag OFF).
    ///
    /// With `CQS_MCP_ENABLE_MUTATIONS` unset, the exposed MCP tool set must
    /// equal the daemon's JSON-args-capable read command set MINUS the withheld
    /// set — 30 read tools (the zero-arg `stats`/`health`, the Phase-2
    /// `where`/`related`/`stale`, the Phase-3 overlay-capable `task`, the
    /// read-only `notes`, the Phase-4 `suggest`/`impact-diff`, and the
    /// fully-scanned function-card `explain` + module card `context`), zero
    /// mutation delta. Fails if a JSON-args command is added/removed without
    /// updating the MCP surface, or if a withheld command leaks into
    /// `tools/list`.
    #[test]
    #[serial_test::serial(mcp_mutations_env)]
    fn tools_list_matches_json_args_registry() {
        let _guard = MutationsEnvGuard::set(false);
        let expected = expected_read_commands();
        let exposed = exposed_commands();
        assert_eq!(
            exposed, expected,
            "MCP tools/list (flag off) must equal the JSON-args read registry minus the \
             withheld set.\nexposed (mapped to commands): {exposed:?}\nexpected: {expected:?}"
        );
        // The flag-off read surface = exactly 30 read tools.
        assert_eq!(
            exposed.len(),
            30,
            "flag-off tools/list must expose 30 tools"
        );
    }

    /// Flag-gating guard: the mutation tools are present IFF
    /// `CQS_MCP_ENABLE_MUTATIONS=1`. With the flag on, the exposed set is the
    /// read set PLUS exactly the three notes mutators AND the fire-and-forget
    /// `index` (34 total).
    #[test]
    #[serial_test::serial(mcp_mutations_env)]
    fn mutation_tools_present_iff_flag_set() {
        // Flag off → no mutation tools.
        {
            let _guard = MutationsEnvGuard::set(false);
            let off = exposed_commands();
            for cmd in ["notes-add", "notes-update", "notes-remove", "index"] {
                assert!(
                    !off.contains(cmd),
                    "flag-off tools/list must NOT contain `{cmd}`"
                );
            }
        }
        // Flag on → read set + the three notes mutators + the index queue tool.
        {
            let _guard = MutationsEnvGuard::set(true);
            let on = exposed_commands();
            for cmd in ["notes-add", "notes-update", "notes-remove", "index"] {
                assert!(on.contains(cmd), "flag-on tools/list must contain `{cmd}`");
            }
            let mut want = expected_read_commands();
            want.insert("notes-add".to_string());
            want.insert("notes-update".to_string());
            want.insert("notes-remove".to_string());
            want.insert("index".to_string());
            assert_eq!(
                on, want,
                "flag-on tools/list must be the read set plus exactly the 3 notes mutators \
                 and the index queue tool"
            );
            assert_eq!(on.len(), 34, "flag-on tools/list must expose 34 tools");
        }
    }

    /// Destructive-set-absent guard: `gc` / `slot remove` / `index --force` /
    /// `model swap` / `cache clear` must NEVER appear in `tools/list`,
    /// REGARDLESS of the mutation flag — boundary by absence (§1.3, §2.2). No
    /// flag re-enables the destructive set in Phase 2b.
    ///
    /// Note: `cqs_index` (the non-destructive fire-and-forget queue) IS exposed
    /// when the flag is on — it is NOT in this list. The withheld variant is the
    /// FORCED full rebuild, which has no tool name at all (the scoped `IndexArgs`
    /// core exposes no `force` field), so there is nothing to assert-absent for
    /// it beyond the absence of any `--force` reachability (covered by the
    /// `index --force` unreachability test).
    #[test]
    #[serial_test::serial(mcp_mutations_env)]
    fn destructive_set_absent_regardless_of_flag() {
        let destructive_tools = [
            "cqs_gc",
            "cqs_slot_remove",
            "cqs_index_force",
            "cqs_model_swap",
            "cqs_cache_clear",
            "cqs_audit_mode",
        ];
        for on in [false, true] {
            let _guard = MutationsEnvGuard::set(on);
            let names: Vec<&'static str> = tools::tool_table().iter().map(|t| t.name).collect();
            for d in destructive_tools {
                assert!(
                    !names.contains(&d),
                    "destructive tool `{d}` must be absent from tools/list (flag={on})"
                );
            }
        }
    }

    /// `index --force` is unreachable as a tool regardless of the flag: the only
    /// `index` tool exposed is the non-destructive queue (`cqs_index`), and its
    /// scoped `IndexArgs` core has NO `force` field — so a `force` key in
    /// `arguments` is simply ignored (the core does not deserialize it), and
    /// there is no separate forced-rebuild tool. This pins the §1.3 withhold.
    #[test]
    #[serial_test::serial(mcp_mutations_env)]
    fn index_force_is_unreachable_as_a_tool() {
        let _guard = MutationsEnvGuard::set(true);
        let names: Vec<&'static str> = tools::tool_table().iter().map(|t| t.name).collect();
        // Exactly one `index` tool — the queue — and no forced variant.
        let index_tools: Vec<&&'static str> =
            names.iter().filter(|n| n.contains("index")).collect();
        assert_eq!(
            index_tools.len(),
            1,
            "exactly one index tool must be exposed (the non-destructive queue), got: {index_tools:?}"
        );
        assert!(
            names.contains(&"cqs_index"),
            "the exposed index tool must be the non-destructive `cqs_index` queue"
        );
        // The exposed `cqs_index` schema must NOT advertise a `force` property —
        // the destructive flag is withheld by absence from the scoped core.
        let listed = tools::list();
        let arr = listed
            .get("tools")
            .and_then(|t| t.as_array())
            .expect("tools array");
        let index_schema = arr
            .iter()
            .find(|t| t.get("name").and_then(|n| n.as_str()) == Some("cqs_index"))
            .and_then(|t| t.get("inputSchema"))
            .and_then(|s| s.get("properties"))
            .and_then(|p| p.as_object())
            .expect("cqs_index inputSchema properties");
        assert!(
            !index_schema.contains_key("force"),
            "cqs_index schema must not expose a `force` property (destructive variant withheld), \
             got properties: {:?}",
            index_schema.keys().collect::<Vec<_>>()
        );
    }

    /// Every exposed tool carries the `cqs_` prefix (D1), an `inputSchema` that
    /// is a non-empty JSON object, and a non-empty description. Runs with the
    /// mutation flag ON so the notes-mutator rows are covered too.
    #[test]
    #[serial_test::serial(mcp_mutations_env)]
    fn every_tool_is_well_formed() {
        let _guard = MutationsEnvGuard::set(true);
        for t in tools::tool_table() {
            assert!(
                t.name.starts_with("cqs_"),
                "tool `{}` must carry the `cqs_` prefix (D1)",
                t.name
            );
            let schema = (t.input_schema)();
            assert_eq!(
                schema.get("type").and_then(|v| v.as_str()),
                Some("object"),
                "tool `{}` inputSchema must be a JSON object",
                t.name
            );
            assert!(
                schema.get("properties").is_some(),
                "tool `{}` inputSchema must carry a `properties` object",
                t.name
            );
            assert!(
                !t.description.is_empty(),
                "tool `{}` must carry a non-empty description",
                t.name
            );
        }
    }

    /// Annotation honesty (§3): the exposed annotations in `tools/list` match
    /// the per-command table. Read tools are read-only/idempotent/
    /// non-destructive; `notes_add` is additive (mutating, non-idempotent,
    /// non-destructive); `notes_update` is idempotent but mutating;
    /// `notes_remove` is the lone `destructiveHint:true`.
    #[test]
    #[serial_test::serial(mcp_mutations_env)]
    fn tool_annotations_match_table() {
        let _guard = MutationsEnvGuard::set(true);
        let listed = tools::list();
        let arr = listed
            .get("tools")
            .and_then(|t| t.as_array())
            .expect("tools array");
        let find = |name: &str| -> serde_json::Value {
            arr.iter()
                .find(|t| t.get("name").and_then(|n| n.as_str()) == Some(name))
                .and_then(|t| t.get("annotations").cloned())
                .unwrap_or_else(|| panic!("tool `{name}` not in tools/list"))
        };
        let hint = |a: &serde_json::Value, k: &str| a.get(k).and_then(|v| v.as_bool());

        // A representative read tool — the read quartet.
        let search = find("cqs_search");
        assert_eq!(hint(&search, "readOnlyHint"), Some(true));
        assert_eq!(hint(&search, "destructiveHint"), Some(false));
        assert_eq!(hint(&search, "openWorldHint"), Some(false));

        let add = find("cqs_notes_add");
        assert_eq!(hint(&add, "readOnlyHint"), Some(false));
        assert_eq!(hint(&add, "destructiveHint"), Some(false));
        assert_eq!(hint(&add, "idempotentHint"), Some(false));

        let update = find("cqs_notes_update");
        assert_eq!(hint(&update, "readOnlyHint"), Some(false));
        assert_eq!(hint(&update, "destructiveHint"), Some(false));
        assert_eq!(hint(&update, "idempotentHint"), Some(true));

        let remove = find("cqs_notes_remove");
        assert_eq!(hint(&remove, "readOnlyHint"), Some(false));
        assert_eq!(
            hint(&remove, "destructiveHint"),
            Some(true),
            "notes_remove is the lone destructiveHint:true in the exposed set"
        );

        // The Phase-2b queue tool: a mutator that is idempotent (repeated calls
        // coalesce into one watch-loop walk) and non-destructive (rebuilds from
        // the source tree).
        let index = find("cqs_index");
        assert_eq!(hint(&index, "readOnlyHint"), Some(false));
        assert_eq!(hint(&index, "idempotentHint"), Some(true));
        assert_eq!(
            hint(&index, "destructiveHint"),
            Some(false),
            "cqs_index is a non-destructive queued reindex"
        );
        assert_eq!(hint(&index, "openWorldHint"), Some(false));
    }

    /// The doc/signature relay tools `context` and `explain` — both now fully
    /// injection-scanned (the context.rs / explain.rs RT-RELAY scan==relayed
    /// guards pin per-chunk completeness) — must be PRESENT in the exposed
    /// surface, regardless of the mutation flag. A regression guard against
    /// accidentally re-withholding a cleared relay tool. (The destructive set is
    /// the one held back by absence — see `destructive_set_absent_regardless_of_flag`.)
    #[test]
    #[serial_test::serial(mcp_mutations_env)]
    fn doc_signature_relay_tools_exposed() {
        for on in [false, true] {
            let _guard = MutationsEnvGuard::set(on);
            let names: Vec<&'static str> = tools::tool_table().iter().map(|t| t.name).collect();
            assert!(
                names.contains(&"cqs_context"),
                "cqs_context must be exposed in tools/list (flag={on})"
            );
            assert!(
                names.contains(&"cqs_explain"),
                "cqs_explain must be exposed in tools/list (flag={on})"
            );
        }
    }

    /// Schema ↔ wire consistency: a tool whose `inputSchema` advertises a
    /// non-empty `required` array carries a core with at least one field that
    /// genuinely lacks a serde default (e.g. `plan.description`,
    /// `callers.name`). The MCP `required` list is honest — it must reflect what
    /// the daemon will actually reject when omitted. This guards against a
    /// schema that claims a field is optional while the daemon errors on its
    /// absence (or vice versa), which would silently mislead the model.
    ///
    /// The invariant is checked structurally: every `required` entry must be a
    /// declared property of the same schema. (A fully-`serde(default)` core like
    /// `QueryArgs` has no `required` array at all — that path is trivially
    /// consistent.)
    #[test]
    #[serial_test::serial(mcp_mutations_env)]
    fn tool_required_fields_are_declared_properties() {
        let _guard = MutationsEnvGuard::set(true);
        for t in tools::tool_table() {
            let schema = (t.input_schema)();
            let props = schema
                .get("properties")
                .and_then(|p| p.as_object())
                .unwrap_or_else(|| panic!("tool `{}`: schema has no properties", t.name));
            if let Some(req) = schema.get("required") {
                let arr = req
                    .as_array()
                    .unwrap_or_else(|| panic!("tool `{}`: `required` not an array", t.name));
                for field in arr {
                    let name = field.as_str().unwrap_or_else(|| {
                        panic!("tool `{}`: `required` entry not a string", t.name)
                    });
                    assert!(
                        props.contains_key(name),
                        "tool `{}`: required field `{name}` is not a declared property",
                        t.name
                    );
                }
            }
        }
    }

    /// The read-tool MCP names in registry order, with the mutation flag OFF
    /// (so `tool_table` returns exactly the read surface). The single source of
    /// truth the README list must mirror.
    fn read_tool_names_in_order() -> Vec<&'static str> {
        let _guard = MutationsEnvGuard::set(false);
        tools::tool_table().iter().map(|t| t.name).collect()
    }

    /// Extract the `cqs_*` tokens from the README's "Default (read-only)" tool
    /// bullet, in document order. Reads the README at the crate root via
    /// `CARGO_MANIFEST_DIR` so the test runs from any working directory.
    fn readme_read_tool_names() -> Vec<String> {
        let readme = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/README.md"))
            .expect("read README.md");
        let full_line = readme
            .lines()
            .find(|l| l.contains("Default (read-only)"))
            .expect("README must carry a `Default (read-only)` MCP tool bullet");
        // The tool list is the prose BEFORE the parenthetical withheld-tools
        // note, which is introduced by a sentence boundary `. (` (the label
        // `(read-only)` earlier on the line uses no preceding `. `). Cut there so
        // the withheld names are not parsed as listed tools.
        let line = full_line.split(". (").next().unwrap_or(full_line);
        // Collect `cqs_<ident>` tokens (backtick-wrapped). A token is `cqs_` plus
        // ASCII identifier chars; stop at the first non-ident byte so a trailing
        // backtick / comma / period is excluded. Skip the bare `cqs_` prefix
        // token (the literal "`cqs_`-prefixed" phrase) — a real tool name has at
        // least one char after the underscore.
        let mut names = Vec::new();
        let bytes = line.as_bytes();
        let mut i = 0;
        while let Some(rel) = line[i..].find("cqs_") {
            let start = i + rel;
            let mut end = start;
            while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
                end += 1;
            }
            let token = &line[start..end];
            if token.len() > "cqs_".len() {
                names.push(token.to_string());
            }
            i = end;
        }
        names
    }

    /// README-parity guard (docs-lie): the README's "Default (read-only)" MCP
    /// tool list must equal the `tool_table()` read-tool names, in registry
    /// order. Pins the doc against the registry so a tool added/renamed/removed
    /// in `read_tools` (or a withheld tool leaking into the prose) fails here
    /// rather than shipping a fabricated list. Mirrors the registry-parity guard
    /// above, but against the README rather than the JSON-args registry.
    #[test]
    #[serial_test::serial(mcp_mutations_env)]
    fn readme_read_tool_list_matches_registry() {
        let registry = read_tool_names_in_order();
        let readme = readme_read_tool_names();
        let registry_owned: Vec<String> = registry.iter().map(|s| s.to_string()).collect();
        assert_eq!(
            readme, registry_owned,
            "README `Default (read-only)` MCP tool list must equal the `tool_table()` read \
             tools in registry order.\nREADME: {readme:?}\nregistry: {registry_owned:?}"
        );
    }
}
