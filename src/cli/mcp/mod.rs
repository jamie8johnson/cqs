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

#[cfg(test)]
mod tests {
    use super::tools;

    /// `tools/list`-matches-registry guard.
    ///
    /// The exposed MCP tool set must equal the daemon's JSON-args-capable
    /// command set (the 20 commands that carry a Phase-0 JsonSchema core, per
    /// `cli::batch::json_args::build_batch_cmd`) MINUS the explicitly withheld
    /// set (`context`, `explain` — RT-RELAY doc/signature scan gap, D4b). This
    /// fails if a JSON-args command is added or removed without updating the
    /// MCP surface, or if a withheld command leaks into `tools/list`.
    #[test]
    fn tools_list_matches_json_args_registry() {
        // The canonical JSON-args-capable command set, mirrored from
        // `build_batch_cmd`'s match arms. If Lane 1 adds a command there, this
        // list must grow with it (and the tool table in `tools.rs`).
        let json_args_capable: std::collections::BTreeSet<&str> = [
            "search", "gather", "scout", "onboard", "similar", "callers", "callees", "deps",
            "impact", "test-map", "trace", "blame", "context", "diff", "drift", "dead", "ci",
            "review", "plan", "read",
        ]
        .into_iter()
        .collect();

        // The P1 withheld set (D4b): unscanned doc/signature relay surfaces.
        let withheld: std::collections::BTreeSet<&str> =
            ["context", "explain"].into_iter().collect();

        let expected: std::collections::BTreeSet<String> = json_args_capable
            .difference(&withheld)
            .map(|s| s.to_string())
            .collect();

        let exposed: std::collections::BTreeSet<String> = tools::tool_table()
            .iter()
            .map(|t| {
                let name = t.name.to_string();
                // The MCP tool name carries the `cqs_` prefix (D1) and uses
                // underscores; map it back to the bare daemon command name for
                // the registry comparison. `test-map` is the lone hyphenated
                // command — its MCP name is `cqs_test_map`.
                tools::mcp_name_to_command(&name)
            })
            .collect();

        assert_eq!(
            exposed, expected,
            "MCP tools/list set must equal the JSON-args registry minus the withheld set.\n\
             exposed (mapped to commands): {exposed:?}\n\
             expected:                     {expected:?}"
        );
    }

    /// Every exposed tool carries the `cqs_` prefix (D1), an `inputSchema` that
    /// is a non-empty JSON object, and a `readOnly` annotation (all P1 tools
    /// are reads).
    #[test]
    fn every_tool_is_well_formed() {
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

    /// `context`/`explain` must be ABSENT from the exposed surface (D4b).
    #[test]
    fn withheld_tools_absent() {
        let names: Vec<&'static str> = tools::tool_table().iter().map(|t| t.name).collect();
        assert!(
            !names.contains(&"cqs_context"),
            "cqs_context must be withheld from P1 tools/list"
        );
        assert!(
            !names.contains(&"cqs_explain"),
            "cqs_explain must be withheld from P1 tools/list"
        );
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
    fn tool_required_fields_are_declared_properties() {
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
}
