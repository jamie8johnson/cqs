//! Pipeline execution — chaining batch commands via `|` syntax.

use std::collections::HashSet;

use clap::Parser;

use super::commands::{dispatch, BatchInput};
use super::BatchContext;

/// Maximum names extracted per pipeline stage to prevent fan-out explosion.
/// A 3-stage pipeline dispatches at most 1 + 50 + 50 = 101 calls.
const PIPELINE_FAN_OUT_LIMIT: usize = 50;

/// Check if a command (first token) can receive piped names.
///
/// Parses the tokens with a dummy name arg to get a `BatchCmd`, then checks
/// `is_pipeable()`. Falls back to false on parse failure.
fn is_pipeable_command(tokens: &[String]) -> bool {
    // Try parsing with a dummy name to see if the command is valid and pipeable
    let probe_tokens = if tokens.len() == 1 {
        vec![tokens[0].clone(), "__probe__".to_string()]
    } else {
        tokens.to_vec()
    };
    BatchInput::try_parse_from(&probe_tokens)
        .map(|input| input.cmd.is_pipeable())
        .unwrap_or(false)
}

/// List of pipeable command names for error messages.
fn pipeable_command_names() -> String {
    // Derive from BatchCmd variants — kept in sync via is_pipeable()
    use super::commands::BatchCmd;
    let all = [
        (
            "blame",
            BatchCmd::Blame {
                name: String::new(),
                depth: 10,
                callers: false,
            },
        ),
        (
            "callers",
            BatchCmd::Callers {
                name: String::new(),
            },
        ),
        (
            "callees",
            BatchCmd::Callees {
                name: String::new(),
            },
        ),
        (
            "deps",
            BatchCmd::Deps {
                name: String::new(),
                reverse: false,
            },
        ),
        (
            "explain",
            BatchCmd::Explain {
                name: String::new(),
                tokens: None,
            },
        ),
        (
            "similar",
            BatchCmd::Similar {
                target: String::new(),
                limit: 5,
                threshold: 0.3,
            },
        ),
        (
            "impact",
            BatchCmd::Impact {
                name: String::new(),
                depth: 1,
                suggest_tests: false,
                include_types: false,
            },
        ),
        (
            "test-map",
            BatchCmd::TestMap {
                name: String::new(),
                depth: 5,
            },
        ),
        (
            "related",
            BatchCmd::Related {
                name: String::new(),
                limit: 5,
            },
        ),
        (
            "scout",
            BatchCmd::Scout {
                query: String::new(),
                limit: 10,
                tokens: None,
            },
        ),
    ];
    all.iter()
        .filter(|(_, cmd)| cmd.is_pipeable())
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Extract function/chunk names from a dispatch result JSON value.
///
/// Tries each extractor in order: bare array, standard named fields, scout
/// nested groups, plus the top-level "name" field (explain). Deduplicates
/// preserving order.
fn extract_names(val: &serde_json::Value) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut names = Vec::new();

    let mut push = |n: &str| {
        if !n.is_empty() && seen.insert(n.to_string()) {
            names.push(n.to_string());
        }
    };

    // Top-level "name" (explain returns the target's own name)
    if let Some(name) = val.get("name").and_then(|v| v.as_str()) {
        push(name);
    }

    // Bare array (callers returns [...]) — early return, nothing else to check
    if val.is_array() {
        for n in extract_from_bare_array(val) {
            push(&n);
        }
        return names;
    }

    // Known named array fields (search, gather, impact, dead, trace, related, explain)
    for n in extract_from_standard_fields(val) {
        push(&n);
    }

    // Scout: nested file_groups[].chunks[].name
    for n in extract_from_scout_groups(val) {
        push(&n);
    }

    names
}

/// Extract names from a bare JSON array (e.g., callers returns `[{name: ...}, ...]`).
fn extract_from_bare_array(val: &serde_json::Value) -> Vec<String> {
    val.as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| item.get("name")?.as_str().map(String::from))
        .collect()
}

/// Extract names from any array fields in a JSON object that contain objects with "name" fields.
///
/// Instead of probing a hardcoded list of field names, walks all top-level object
/// values looking for arrays of objects with a `"name"` string field. This is
/// key-agnostic: adding a new command with a `"foo": [{"name": ...}]` field
/// automatically works without updating a constant.
fn extract_from_standard_fields(val: &serde_json::Value) -> Vec<String> {
    let obj = match val.as_object() {
        Some(o) => o,
        None => return Vec::new(),
    };

    let mut names = Vec::new();
    for (key, field_val) in obj {
        // Skip "name" at top level (handled by caller) and non-array fields
        if key == "name" {
            continue;
        }
        if let Some(arr) = field_val.as_array() {
            for item in arr {
                if let Some(name) = item.get("name").and_then(|v| v.as_str()) {
                    names.push(name.to_string());
                }
            }
        }
    }
    names
}

/// Extract names from scout's nested `file_groups[].chunks[].name` structure.
fn extract_from_scout_groups(val: &serde_json::Value) -> Vec<String> {
    val.get("file_groups")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .flat_map(|group| {
            group
                .get("chunks")
                .and_then(|v| v.as_array())
                .into_iter()
                .flatten()
                .filter_map(|chunk| chunk.get("name")?.as_str().map(String::from))
        })
        .collect()
}

/// Split a token list by standalone `|` into pipeline segments.
fn split_tokens_by_pipe(tokens: &[String]) -> Vec<Vec<String>> {
    let mut segments: Vec<Vec<String>> = Vec::new();
    let mut current: Vec<String> = Vec::new();

    for token in tokens {
        if token == "|" {
            segments.push(std::mem::take(&mut current));
        } else {
            current.push(token.clone());
        }
    }
    segments.push(current);
    segments
}

/// Execute a pipeline: chain commands where upstream names feed downstream.
pub(crate) fn execute_pipeline(
    ctx: &BatchContext,
    tokens: &[String],
    raw_line: &str,
) -> serde_json::Value {
    let _span = tracing::info_span!("pipeline", input = raw_line).entered();

    let segments = split_tokens_by_pipe(tokens);
    let stage_count = segments.len();

    // Validate: no empty segments
    for (i, seg) in segments.iter().enumerate() {
        if seg.is_empty() {
            return serde_json::json!({"error": format!(
                "Empty pipeline segment at position {}", i + 1
            )});
        }
    }

    // Validate: downstream segments (index 1+) must be pipeable
    for seg in &segments[1..] {
        if !is_pipeable_command(seg) {
            let cmd = seg.first().map(|s| s.as_str()).unwrap_or("(empty)");
            return serde_json::json!({"error": format!(
                "Cannot pipe into '{}' \u{2014} it doesn't accept a function name. \
                 Pipeable commands: {}",
                cmd,
                pipeable_command_names()
            )});
        }
    }

    // Validate: quit/exit/help not allowed in pipelines
    for seg in &segments {
        if let Some(first) = seg.first() {
            let lower = first.to_ascii_lowercase();
            if lower == "quit" || lower == "exit" || lower == "help" {
                return serde_json::json!({"error": format!(
                    "'{}' cannot be used in a pipeline", first
                )});
            }
        }
    }

    // Stage 0: execute first segment normally
    let stage0_result = {
        let _stage_span = tracing::info_span!(
            "pipeline_stage",
            stage = 0,
            command = segments[0].first().map(|s| s.as_str()).unwrap_or("?"),
        )
        .entered();

        match BatchInput::try_parse_from(&segments[0]) {
            Ok(input) => match dispatch(ctx, input.cmd) {
                Ok(val) => val,
                Err(e) => {
                    return serde_json::json!({"error": format!(
                        "Pipeline stage 1 failed: {}", e
                    )});
                }
            },
            Err(e) => {
                return serde_json::json!({"error": format!(
                    "Pipeline stage 1 parse error: {}", e
                )});
            }
        }
    };

    // Process remaining stages
    let mut current_value = stage0_result;
    let mut any_truncated = false;

    for (stage_idx, segment) in segments[1..].iter().enumerate() {
        let stage_num = stage_idx + 1; // 1-indexed for display (stage 0 already done)

        // Extract names from current result
        let mut names = extract_names(&current_value);
        tracing::debug!(stage = stage_num, count = names.len(), "Names extracted");

        if names.len() > PIPELINE_FAN_OUT_LIMIT {
            any_truncated = true;
            tracing::info!(
                stage = stage_num,
                original = names.len(),
                limit = PIPELINE_FAN_OUT_LIMIT,
                "Fan-out truncated"
            );
            names.truncate(PIPELINE_FAN_OUT_LIMIT);
        }

        let total_inputs = names.len();
        let _stage_span = tracing::info_span!(
            "pipeline_stage",
            stage = stage_num + 1, // 1-based for user
            command = segment.first().map(|s| s.as_str()).unwrap_or("?"),
            fan_out = total_inputs,
        )
        .entered();

        if names.is_empty() {
            // No names to fan out — return empty pipeline result
            return build_pipeline_result(raw_line, stage_count, vec![], vec![], 0, false);
        }

        let mut results: Vec<(String, serde_json::Value)> = Vec::new();
        let mut errors: Vec<(String, String)> = Vec::new();

        for name in &names {
            // Build tokens: prepend name to downstream segment
            let mut cmd_tokens = vec![segment[0].clone(), name.clone()];
            cmd_tokens.extend_from_slice(&segment[1..]);

            match BatchInput::try_parse_from(&cmd_tokens) {
                Ok(input) => match dispatch(ctx, input.cmd) {
                    Ok(val) => results.push((name.clone(), val)),
                    Err(e) => {
                        tracing::warn!(name = name, error = %e, "Per-name dispatch failed");
                        errors.push((name.clone(), e.to_string()));
                    }
                },
                Err(e) => {
                    tracing::warn!(name = name, error = %e, "Per-name parse failed");
                    errors.push((name.clone(), e.to_string()));
                }
            }
        }

        // If this is the last stage, build the pipeline result envelope
        if stage_num == segments.len() - 1 {
            return build_pipeline_result(
                raw_line,
                stage_count,
                results,
                errors,
                total_inputs,
                any_truncated,
            );
        }

        // Intermediate stage: merge results for next stage's name extraction.
        // Cap at PIPELINE_FAN_OUT_LIMIT to avoid unbounded intermediate growth.
        let mut merged_names: Vec<String> = Vec::new();
        let mut merged_seen = HashSet::new();
        'merge: for (_, val) in &results {
            for n in extract_names(val) {
                if merged_seen.insert(n.clone()) {
                    merged_names.push(n);
                    if merged_names.len() >= PIPELINE_FAN_OUT_LIMIT {
                        break 'merge;
                    }
                }
            }
        }

        // Build a synthetic value with a "results" array for extraction
        let synthetic: Vec<serde_json::Value> = merged_names
            .iter()
            .map(|n| serde_json::json!({"name": n}))
            .collect();
        current_value = serde_json::json!({"results": synthetic});
    }

    // Should not reach here, but safety net
    serde_json::json!({"error": "Pipeline execution ended unexpectedly"})
}

/// Build the final pipeline result envelope.
fn build_pipeline_result(
    pipeline_str: &str,
    stages: usize,
    results: Vec<(String, serde_json::Value)>,
    errors: Vec<(String, String)>,
    total_inputs: usize,
    truncated: bool,
) -> serde_json::Value {
    let results_json: Vec<serde_json::Value> = results
        .into_iter()
        .map(|(input, data)| serde_json::json!({"_input": input, "data": data}))
        .collect();

    let errors_json: Vec<serde_json::Value> = errors
        .into_iter()
        .map(|(input, err)| serde_json::json!({"_input": input, "error": err}))
        .collect();

    serde_json::json!({
        "pipeline": pipeline_str,
        "stages": stages,
        "results": results_json,
        "errors": errors_json,
        "total_inputs": total_inputs,
        "truncated": truncated,
    })
}

/// Check if a token list contains a pipeline (standalone `|` token).
pub(crate) fn has_pipe_token(tokens: &[String]) -> bool {
    tokens.iter().any(|t| t == "|")
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn test_extract_names_search_result() {
        let val = serde_json::json!({
            "results": [{"name": "a", "file": "f.rs"}, {"name": "b", "file": "g.rs"}],
            "query": "test",
            "total": 2
        });
        assert_eq!(extract_names(&val), vec!["a", "b"]);
    }

    #[test]
    fn test_extract_names_callers_bare_array() {
        let val = serde_json::json!([{"name": "a", "file": "f.rs"}, {"name": "b", "file": "g.rs"}]);
        assert_eq!(extract_names(&val), vec!["a", "b"]);
    }

    #[test]
    fn test_extract_names_callees() {
        let val = serde_json::json!({
            "function": "f",
            "calls": [{"name": "a", "line": 1}],
            "count": 1
        });
        assert_eq!(extract_names(&val), vec!["a"]);
    }

    #[test]
    fn test_extract_names_impact() {
        let val = serde_json::json!({
            "function": "f",
            "callers": [{"name": "a"}],
            "tests": [{"name": "b"}],
            "caller_count": 1,
            "test_count": 1
        });
        let names = extract_names(&val);
        assert!(names.contains(&"a".to_string()));
        assert!(names.contains(&"b".to_string()));
    }

    #[test]
    fn test_extract_names_dead() {
        let val = serde_json::json!({
            "dead": [{"name": "a"}],
            "possibly_dead_pub": [{"name": "b"}],
            "total_dead": 1,
            "total_possibly_dead_pub": 1
        });
        let names = extract_names(&val);
        assert!(names.contains(&"a".to_string()));
        assert!(names.contains(&"b".to_string()));
    }

    #[test]
    fn test_extract_names_related() {
        let val = serde_json::json!({
            "target": "f",
            "shared_callers": [{"name": "a"}],
            "shared_callees": [{"name": "b"}],
            "shared_types": [{"name": "c"}]
        });
        let names = extract_names(&val);
        assert!(names.contains(&"a".to_string()));
        assert!(names.contains(&"b".to_string()));
        assert!(names.contains(&"c".to_string()));
    }

    #[test]
    fn test_extract_names_trace() {
        let val = serde_json::json!({
            "source": "s",
            "target": "t",
            "path": [{"name": "a"}, {"name": "b"}],
            "depth": 1
        });
        let names = extract_names(&val);
        assert!(names.contains(&"a".to_string()));
        assert!(names.contains(&"b".to_string()));
    }

    #[test]
    fn test_extract_names_explain() {
        let val = serde_json::json!({
            "name": "target",
            "callers": [{"name": "a"}],
            "similar": [{"name": "b"}]
        });
        let names = extract_names(&val);
        assert_eq!(names[0], "target"); // top-level name first
        assert!(names.contains(&"a".to_string()));
        assert!(names.contains(&"b".to_string()));
    }

    #[test]
    fn test_extract_names_empty_results() {
        let val = serde_json::json!({"results": [], "query": "x", "total": 0});
        assert!(extract_names(&val).is_empty());
    }

    #[test]
    fn test_extract_names_stats_no_names() {
        let val = serde_json::json!({
            "total_chunks": 100,
            "total_files": 10,
            "notes": 5
        });
        assert!(extract_names(&val).is_empty());
    }

    #[test]
    fn test_extract_names_dedup() {
        let val = serde_json::json!({
            "results": [{"name": "a"}, {"name": "a"}, {"name": "b"}]
        });
        assert_eq!(extract_names(&val), vec!["a", "b"]);
    }

    #[test]
    fn test_is_pipeable_callers() {
        assert!(is_pipeable_command(&["callers".to_string()]));
    }

    #[test]
    fn test_is_pipeable_search() {
        assert!(!is_pipeable_command(&[
            "search".to_string(),
            "foo".to_string()
        ]));
    }

    #[test]
    fn test_is_pipeable_stats() {
        assert!(!is_pipeable_command(&["stats".to_string()]));
    }

    #[test]
    fn test_split_tokens_by_pipe() {
        let tokens: Vec<String> = vec!["search", "foo", "|", "callers"]
            .into_iter()
            .map(String::from)
            .collect();
        let segments = split_tokens_by_pipe(&tokens);
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0], vec!["search", "foo"]);
        assert_eq!(segments[1], vec!["callers"]);
    }

    #[test]
    fn test_split_tokens_three_stages() {
        let tokens: Vec<String> = vec!["search", "foo", "|", "callers", "|", "test-map"]
            .into_iter()
            .map(String::from)
            .collect();
        let segments = split_tokens_by_pipe(&tokens);
        assert_eq!(segments.len(), 3);
        assert_eq!(segments[0], vec!["search", "foo"]);
        assert_eq!(segments[1], vec!["callers"]);
        assert_eq!(segments[2], vec!["test-map"]);
    }

    #[test]
    fn test_has_pipe_token() {
        let with_pipe: Vec<String> = vec!["search", "foo", "|", "callers"]
            .into_iter()
            .map(String::from)
            .collect();
        assert!(has_pipe_token(&with_pipe));

        let without_pipe: Vec<String> = vec!["search", "foo|bar"]
            .into_iter()
            .map(String::from)
            .collect();
        assert!(!has_pipe_token(&without_pipe));
    }

    #[test]
    fn test_extract_names_scout() {
        let val = serde_json::json!({
            "file_groups": [
                {
                    "file": "src/search.rs",
                    "chunks": [
                        {"name": "search_filtered", "role": "modify_target"},
                        {"name": "resolve_target", "role": "dependency"}
                    ]
                },
                {
                    "file": "src/store.rs",
                    "chunks": [
                        {"name": "open_store", "role": "modify_target"}
                    ]
                }
            ],
            "summary": {"total_files": 2}
        });
        let names = extract_names(&val);
        assert_eq!(names.len(), 3);
        assert!(names.contains(&"search_filtered".to_string()));
        assert!(names.contains(&"resolve_target".to_string()));
        assert!(names.contains(&"open_store".to_string()));
    }

    #[test]
    fn test_is_pipeable_scout() {
        assert!(is_pipeable_command(&[
            "scout".to_string(),
            "foo".to_string()
        ]));
    }

    /// Verify every pipeable command accepts a function name as first arg.
    /// If you add a new command variant, set `is_pipeable()` on BatchCmd accordingly.
    #[test]
    fn test_pipeable_commands_parse_with_name_arg() {
        let pipeable = [
            "blame", "callers", "callees", "deps", "explain", "similar", "impact", "test-map",
            "related", "scout",
        ];
        for cmd in pipeable {
            let result = BatchInput::try_parse_from([cmd, "test_function"]);
            assert!(
                result.is_ok(),
                "Pipeable command '{cmd}' should accept a positional name arg"
            );
            let input = result.unwrap();
            assert!(
                input.cmd.is_pipeable(),
                "'{cmd}' should be pipeable via is_pipeable()"
            );
        }
    }

    /// Non-pipeable commands should return false from is_pipeable().
    #[test]
    fn test_non_pipeable_not_pipeable() {
        let non_pipeable_cmds = [
            ("search", vec!["search", "foo"]),
            ("gather", vec!["gather", "foo"]),
            ("dead", vec!["dead"]),
            ("stats", vec!["stats"]),
            ("stale", vec!["stale"]),
            ("health", vec!["health"]),
            ("context", vec!["context", "path"]),
        ];
        for (label, tokens) in non_pipeable_cmds {
            let tokens: Vec<String> = tokens.into_iter().map(String::from).collect();
            let result = BatchInput::try_parse_from(&tokens);
            if let Ok(input) = result {
                assert!(!input.cmd.is_pipeable(), "'{label}' should NOT be pipeable");
            }
        }
    }
}
