//! JSON and Mermaid serialization for impact results

use std::path::Path;

use super::types::{DiffImpactResult, ImpactResult};

/// Serialize impact result to JSON, relativizing paths against the project root
pub fn impact_to_json(result: &ImpactResult, root: &Path) -> serde_json::Value {
    let callers_json: Vec<_> = result
        .callers
        .iter()
        .map(|c| {
            let rel = crate::rel_display(&c.file, root);
            serde_json::json!({
                "name": c.name,
                "file": rel,
                "line": c.line,
                "call_line": c.call_line,
                "snippet": c.snippet,
            })
        })
        .collect();

    let tests_json: Vec<_> = result
        .tests
        .iter()
        .map(|t| {
            let rel = crate::rel_display(&t.file, root);
            serde_json::json!({
                "name": t.name,
                "file": rel,
                "line": t.line,
                "call_depth": t.call_depth,
            })
        })
        .collect();

    let mut output = serde_json::json!({
        "function": result.function_name,
        "callers": callers_json,
        "tests": tests_json,
        "caller_count": callers_json.len(),
        "test_count": tests_json.len(),
    });

    if !result.transitive_callers.is_empty() {
        let trans_json: Vec<_> = result
            .transitive_callers
            .iter()
            .map(|c| {
                let rel = crate::rel_display(&c.file, root);
                serde_json::json!({
                    "name": c.name,
                    "file": rel,
                    "line": c.line,
                    "depth": c.depth,
                })
            })
            .collect();
        if let Some(obj) = output.as_object_mut() {
            obj.insert("transitive_callers".into(), serde_json::json!(trans_json));
        }
    }

    output
}

/// Generate a mermaid diagram from impact result
pub fn impact_to_mermaid(result: &ImpactResult, root: &Path) -> String {
    let mut lines = vec!["graph TD".to_string()];
    lines.push(format!(
        "    A[\"{}\"]\n    style A fill:#f96",
        mermaid_escape(&result.function_name)
    ));

    let mut idx = 1;
    for c in &result.callers {
        let rel = crate::rel_display(&c.file, root);
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
        let rel = crate::rel_display(&t.file, root);
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

    lines.join("\n")
}

/// Serialize diff impact result to JSON
pub fn diff_impact_to_json(result: &DiffImpactResult, root: &Path) -> serde_json::Value {
    let changed_json: Vec<_> = result
        .changed_functions
        .iter()
        .map(|f| {
            serde_json::json!({
                "name": f.name,
                "file": f.file,
                "line_start": f.line_start,
            })
        })
        .collect();

    let callers_json: Vec<_> = result
        .all_callers
        .iter()
        .map(|c| {
            let rel = crate::rel_display(&c.file, root);
            serde_json::json!({
                "name": c.name,
                "file": rel,
                "line": c.line,
                "call_line": c.call_line,
            })
        })
        .collect();

    let tests_json: Vec<_> = result
        .all_tests
        .iter()
        .map(|t| {
            let rel = crate::rel_display(&t.file, root);
            serde_json::json!({
                "name": t.name,
                "file": rel,
                "line": t.line,
                "via": t.via,
                "call_depth": t.call_depth,
            })
        })
        .collect();

    serde_json::json!({
        "changed_functions": changed_json,
        "callers": callers_json,
        "tests": tests_json,
        "summary": {
            "changed_count": result.summary.changed_count,
            "caller_count": result.summary.caller_count,
            "test_count": result.summary.test_count,
        }
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
        let result = ImpactResult {
            function_name: "target_fn".to_string(),
            callers: vec![CallerDetail {
                name: "caller_a".to_string(),
                file: PathBuf::from("/project/src/lib.rs"),
                line: 10,
                call_line: 15,
                snippet: Some("target_fn()".to_string()),
            }],
            tests: vec![TestInfo {
                name: "test_target".to_string(),
                file: PathBuf::from("/project/tests/test.rs"),
                line: 1,
                call_depth: 2,
            }],
            transitive_callers: Vec::new(),
        };
        let root = Path::new("/project");
        let json = impact_to_json(&result, root);

        assert_eq!(json["function"], "target_fn");
        assert_eq!(json["caller_count"], 1);
        assert_eq!(json["test_count"], 1);

        let callers = json["callers"].as_array().unwrap();
        assert_eq!(callers[0]["name"], "caller_a");
        assert_eq!(callers[0]["file"], "src/lib.rs");
        assert_eq!(callers[0]["line"], 10);
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
                file: PathBuf::from("/project/src/app.rs"),
                line: 5,
                depth: 2,
            }],
        };
        let root = Path::new("/project");
        let json = impact_to_json(&result, root);

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
        };
        let root = Path::new("/project");
        let json = impact_to_json(&result, root);

        assert_eq!(json["function"], "lonely");
        assert_eq!(json["caller_count"], 0);
        assert_eq!(json["test_count"], 0);
        assert!(json.get("transitive_callers").is_none());
    }

    // ===== diff_impact_to_json tests =====

    #[test]
    fn test_diff_impact_to_json_structure() {
        let result = DiffImpactResult {
            changed_functions: vec![ChangedFunction {
                name: "changed_fn".to_string(),
                file: "src/lib.rs".to_string(),
                line_start: 10,
            }],
            all_callers: vec![CallerDetail {
                name: "caller_a".to_string(),
                file: PathBuf::from("/project/src/app.rs"),
                line: 20,
                call_line: 25,
                snippet: None,
            }],
            all_tests: vec![DiffTestInfo {
                name: "test_changed".to_string(),
                file: PathBuf::from("/project/tests/test.rs"),
                line: 1,
                via: "changed_fn".to_string(),
                call_depth: 1,
            }],
            summary: DiffImpactSummary {
                changed_count: 1,
                caller_count: 1,
                test_count: 1,
            },
        };
        let root = Path::new("/project");
        let json = diff_impact_to_json(&result, root);

        let changed = json["changed_functions"].as_array().unwrap();
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0]["name"], "changed_fn");

        let callers = json["callers"].as_array().unwrap();
        assert_eq!(callers.len(), 1);
        assert_eq!(callers[0]["name"], "caller_a");

        let tests = json["tests"].as_array().unwrap();
        assert_eq!(tests.len(), 1);
        assert_eq!(tests[0]["name"], "test_changed");
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
            },
        };
        let root = Path::new("/project");
        let json = diff_impact_to_json(&result, root);

        assert_eq!(json["changed_functions"].as_array().unwrap().len(), 0);
        assert_eq!(json["callers"].as_array().unwrap().len(), 0);
        assert_eq!(json["tests"].as_array().unwrap().len(), 0);
        assert_eq!(json["summary"]["changed_count"], 0);
    }
}
