//! JSON and Mermaid serialization for impact results

use super::types::{DiffImpactResult, ImpactResult, TestSuggestion};

/// Serialize impact result to JSON.
///
/// Paths in the result are already relative to the project root (set at
/// construction time by `analyze_impact`). Count fields (`caller_count`,
/// `test_count`, `type_impacted_count`) are computed by the custom `Serialize`
/// impl on `ImpactResult`.
pub fn impact_to_json(result: &ImpactResult) -> serde_json::Value {
    serde_json::to_value(result).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "Failed to serialize ImpactResult");
        serde_json::json!({})
    })
}

/// Format test suggestions as JSON values.
///
/// Shared by CLI `cmd_impact` and batch `dispatch_impact` to avoid
/// duplicating the field-mapping logic. Uses typed `Serialize` on
/// `TestSuggestion`.
pub fn format_test_suggestions(suggestions: &[TestSuggestion]) -> Vec<serde_json::Value> {
    let _span = tracing::info_span!("format_test_suggestions", count = suggestions.len()).entered();
    suggestions
        .iter()
        .filter_map(|s| {
            serde_json::to_value(s)
                .map_err(|e| {
                    tracing::warn!(error = %e, "Failed to serialize TestSuggestion");
                    e
                })
                .ok()
        })
        .collect()
}

/// Generate a mermaid diagram from impact result.
///
/// Paths in the result are already relative to the project root.
pub fn impact_to_mermaid(result: &ImpactResult) -> String {
    let mut lines = vec!["graph TD".to_string()];
    lines.push(format!(
        "    A[\"{}\"]\n    style A fill:#f96",
        mermaid_escape(&result.function_name)
    ));

    let mut idx = 1;
    for c in &result.callers {
        let rel = crate::normalize_path(&c.file);
        let letter = node_letter(idx);
        lines.push(format!(
            "    {}[\"{} ({}:{})\"]",
            letter,
            mermaid_escape(&c.name),
            mermaid_escape(&rel),
            c.line
        ));
        lines.push(format!("    {} --> A", letter));
        idx += 1;
    }

    for t in &result.tests {
        let rel = crate::normalize_path(&t.file);
        let letter = node_letter(idx);
        lines.push(format!(
            "    {}{{\"{}\\n{}\\ndepth: {}\"}}",
            letter,
            mermaid_escape(&t.name),
            mermaid_escape(&rel),
            t.call_depth
        ));
        lines.push(format!("    {} -.-> A", letter));
        idx += 1;
    }

    for ti in &result.type_impacted {
        let rel = crate::normalize_path(&ti.file);
        let letter = node_letter(idx);
        let types_str = ti.shared_types.join(", ");
        lines.push(format!(
            "    {}[/\"{} ({}:{})\\nvia: {}\"/]",
            letter,
            mermaid_escape(&ti.name),
            mermaid_escape(&rel),
            ti.line,
            mermaid_escape(&types_str),
        ));
        lines.push(format!("    {} -. type .-> A", letter));
        lines.push(format!("    style {} fill:#9cf", letter));
        idx += 1;
    }

    lines.join("\n")
}

/// Serialize diff impact result to JSON.
///
/// Paths in the result are already relative to the project root.
/// Uses typed `Serialize` on `DiffImpactResult`.
pub fn diff_impact_to_json(result: &DiffImpactResult) -> serde_json::Value {
    serde_json::to_value(result).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "Failed to serialize DiffImpactResult");
        serde_json::json!({})
    })
}

/// CQ-V1.29-5: shared empty-diff JSON envelope.
///
/// Returned by `impact_diff` / `affected` / batch graph handlers when the
/// unified-diff input contains no hunks or no hunks map to indexed
/// functions. Centralizing the shape prevents the four previous copies from
/// drifting (e.g. one handler adding a `summary.truncated` field the others
/// miss). Callers that need an extra field (e.g. `overall_risk: "none"` on
/// the affected command) layer it on top of the returned object.
pub fn diff_impact_empty_json() -> serde_json::Value {
    serde_json::json!({
        "changed_functions": [],
        "callers": [],
        "tests": [],
        "summary": { "changed_count": 0, "caller_count": 0, "test_count": 0 }
    })
}

/// Convert index to spreadsheet-style column label: A..Z, AA..AZ, BA..BZ, ...
///
/// Unlike the previous `A1`, `B1` scheme, this produces valid mermaid node IDs
/// that are unambiguous for any number of nodes.
fn node_letter(mut i: usize) -> String {
    let mut result = String::new();
    loop {
        result.insert(0, (b'A' + (i % 26) as u8) as char);
        if i < 26 {
            break;
        }
        i = i / 26 - 1;
    }
    result
}

fn mermaid_escape(s: &str) -> String {
    s.replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::super::types::*;
    use super::*;
    use std::path::PathBuf;

    // ===== node_letter tests =====

    #[test]
    fn test_node_letter_single_char() {
        assert_eq!(node_letter(0), "A");
        assert_eq!(node_letter(1), "B");
        assert_eq!(node_letter(25), "Z");
    }

    #[test]
    fn test_node_letter_double_char() {
        assert_eq!(node_letter(26), "AA");
        assert_eq!(node_letter(27), "AB");
        assert_eq!(node_letter(51), "AZ");
        assert_eq!(node_letter(52), "BA");
    }

    #[test]
    fn test_node_letter_triple_char() {
        assert_eq!(node_letter(702), "AAA");
    }

    // ===== mermaid_escape tests =====

    #[test]
    fn test_mermaid_escape_quotes() {
        assert_eq!(mermaid_escape("hello \"world\""), "hello &quot;world&quot;");
    }

    #[test]
    fn test_mermaid_escape_angle_brackets() {
        assert_eq!(mermaid_escape("Vec<T>"), "Vec&lt;T&gt;");
    }

    #[test]
    fn test_mermaid_escape_no_special() {
        assert_eq!(mermaid_escape("plain_text"), "plain_text");
    }

    #[test]
    fn test_mermaid_escape_all_special() {
        assert_eq!(mermaid_escape("\"<>\""), "&quot;&lt;&gt;&quot;");
    }

    // ===== impact_to_json tests =====

    #[test]
    fn test_impact_to_json_structure() {
        // Paths are already relative (as produced by analyze_impact)
        let result = ImpactResult {
            function_name: "target_fn".to_string(),
            callers: vec![CallerDetail {
                name: "caller_a".to_string(),
                file: PathBuf::from("src/lib.rs"),
                line: 10,
                call_line: 15,
                snippet: Some("target_fn()".to_string()),
            }],
            tests: vec![TestInfo {
                name: "test_target".to_string(),
                file: PathBuf::from("tests/test.rs"),
                line: 1,
                call_depth: 2,
            }],
            transitive_callers: Vec::new(),
            type_impacted: Vec::new(),
            degraded: false,
        };
        let json = impact_to_json(&result);

        assert_eq!(json["name"], "target_fn");
        assert_eq!(json["caller_count"], 1);
        assert_eq!(json["test_count"], 1);

        let callers = json["callers"].as_array().unwrap();
        assert_eq!(callers[0]["name"], "caller_a");
        assert_eq!(callers[0]["file"], "src/lib.rs");
        assert_eq!(callers[0]["line_start"], 10);
        assert_eq!(callers[0]["call_line"], 15);
        assert_eq!(callers[0]["snippet"], "target_fn()");

        let tests = json["tests"].as_array().unwrap();
        assert_eq!(tests[0]["name"], "test_target");
        assert_eq!(tests[0]["call_depth"], 2);
    }

    #[test]
    fn test_impact_to_json_with_transitive() {
        let result = ImpactResult {
            function_name: "target".to_string(),
            callers: Vec::new(),
            tests: Vec::new(),
            transitive_callers: vec![TransitiveCaller {
                name: "indirect".to_string(),
                file: PathBuf::from("src/app.rs"),
                line: 5,
                depth: 2,
            }],
            type_impacted: Vec::new(),
            degraded: false,
        };
        let json = impact_to_json(&result);

        assert!(json["transitive_callers"].is_array());
        let trans = json["transitive_callers"].as_array().unwrap();
        assert_eq!(trans.len(), 1);
        assert_eq!(trans[0]["name"], "indirect");
        assert_eq!(trans[0]["depth"], 2);
    }

    #[test]
    fn test_impact_to_json_empty() {
        let result = ImpactResult {
            function_name: "lonely".to_string(),
            callers: Vec::new(),
            tests: Vec::new(),
            transitive_callers: Vec::new(),
            type_impacted: Vec::new(),
            degraded: false,
        };
        let json = impact_to_json(&result);

        assert_eq!(json["name"], "lonely");
        assert_eq!(json["caller_count"], 0);
        assert_eq!(json["test_count"], 0);
        assert!(json.get("transitive_callers").is_none());
        assert_eq!(json["type_impacted"].as_array().unwrap().len(), 0);
        assert_eq!(json["type_impacted_count"], 0);
    }

    // ===== diff_impact_to_json tests =====

    #[test]
    fn test_diff_impact_to_json_structure() {
        // Paths are already relative (as produced by analyze_diff_impact)
        let result = DiffImpactResult {
            changed_functions: vec![ChangedFunction {
                name: "changed_fn".to_string(),
                file: PathBuf::from("src/lib.rs"),
                line_start: 10,
            }],
            all_callers: vec![CallerDetail {
                name: "caller_a".to_string(),
                file: PathBuf::from("src/app.rs"),
                line: 20,
                call_line: 25,
                snippet: None,
            }],
            all_tests: vec![DiffTestInfo {
                name: "test_changed".to_string(),
                file: PathBuf::from("tests/test.rs"),
                line: 1,
                via: "changed_fn".to_string(),
                call_depth: 1,
            }],
            summary: DiffImpactSummary {
                changed_count: 1,
                caller_count: 1,
                test_count: 1,
                truncated: false,
                truncated_functions: 0,
                degraded: false,
            },
        };
        let json = diff_impact_to_json(&result);

        let changed = json["changed_functions"].as_array().unwrap();
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0]["name"], "changed_fn");

        let callers = json["callers"].as_array().unwrap();
        assert_eq!(callers.len(), 1);
        assert_eq!(callers[0]["name"], "caller_a");
        assert_eq!(callers[0]["line_start"], 20);

        let tests = json["tests"].as_array().unwrap();
        assert_eq!(tests.len(), 1);
        assert_eq!(tests[0]["name"], "test_changed");
        assert_eq!(tests[0]["line_start"], 1);
        assert_eq!(tests[0]["via"], "changed_fn");
        assert_eq!(tests[0]["call_depth"], 1);

        assert_eq!(json["summary"]["changed_count"], 1);
        assert_eq!(json["summary"]["caller_count"], 1);
        assert_eq!(json["summary"]["test_count"], 1);
    }

    #[test]
    fn test_diff_impact_to_json_empty() {
        let result = DiffImpactResult {
            changed_functions: Vec::new(),
            all_callers: Vec::new(),
            all_tests: Vec::new(),
            summary: DiffImpactSummary {
                changed_count: 0,
                caller_count: 0,
                test_count: 0,
                truncated: false,
                truncated_functions: 0,
                degraded: false,
            },
        };
        let json = diff_impact_to_json(&result);

        assert_eq!(json["changed_functions"].as_array().unwrap().len(), 0);
        assert_eq!(json["callers"].as_array().unwrap().len(), 0);
        assert_eq!(json["tests"].as_array().unwrap().len(), 0);
        assert_eq!(json["summary"]["changed_count"], 0);
    }
}
