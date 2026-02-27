//! Integration tests for health, suggest, and deps CLI commands (TC-18)
//!
//! Uses a graph fixture with call relationships and type dependencies:
//!   src/lib.rs:  main() -> process(), process() -> validate(), process() -> transform()
//!   src/tests.rs: test_process() -> process()
//!   src/types.rs: Config struct used by process()

use assert_cmd::Command;
use predicates::prelude::*;
use serial_test::serial;
use std::fs;
use tempfile::TempDir;

/// Get a Command for the cqs binary.
fn cqs() -> Command {
    #[allow(deprecated)]
    Command::cargo_bin("cqs").expect("Failed to find cqs binary")
}

/// Create a project with call relationships and type dependencies.
fn setup_graph_project() -> TempDir {
    let dir = TempDir::new().expect("Failed to create temp dir");
    let src = dir.path().join("src");
    fs::create_dir(&src).expect("Failed to create src dir");

    fs::write(
        src.join("lib.rs"),
        r#"
pub mod types;

/// Entry point
pub fn main() {
    let data = process(42);
    println!("{}", data);
}

/// Process input through validation and transformation
pub fn process(input: i32) -> String {
    let config = types::Config::default();
    let valid = validate(input, &config);
    if valid {
        transform(input)
    } else {
        String::from("invalid")
    }
}

/// Check if input is positive and within config bounds
fn validate(input: i32, config: &types::Config) -> bool {
    input > 0 && input <= config.max_value
}

/// Double and format the input
fn transform(input: i32) -> String {
    format!("result: {}", input * 2)
}
"#,
    )
    .expect("Failed to write lib.rs");

    fs::write(
        src.join("types.rs"),
        r#"
/// Configuration for processing
#[derive(Default)]
pub struct Config {
    pub max_value: i32,
}

impl Config {
    pub fn new(max: i32) -> Self {
        Config { max_value: max }
    }
}
"#,
    )
    .expect("Failed to write types.rs");

    fs::write(
        src.join("tests.rs"),
        r#"
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_process() {
        let result = process(5);
        assert_eq!(result, "result: 10");
    }
}
"#,
    )
    .expect("Failed to write tests.rs");

    dir
}

/// Initialize and index a project.
fn init_and_index(dir: &TempDir) {
    cqs()
        .args(["init"])
        .current_dir(dir.path())
        .assert()
        .success();

    cqs()
        .args(["index"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Index complete"));
}

// =============================================================================
// Health command (TC-18)
// =============================================================================

#[test]
#[serial]
fn test_health_cli_json() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    let output = cqs()
        .args(["health", "--json"])
        .current_dir(dir.path())
        .assert()
        .success();

    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("Invalid JSON: {} -- raw: {}", e, stdout));

    // Verify expected fields (HealthReport derives Serialize directly)
    assert!(
        parsed["stats"]["total_chunks"].is_number(),
        "health --json should have stats.total_chunks"
    );
    assert!(
        parsed["stats"]["total_files"].is_number(),
        "health --json should have stats.total_files"
    );
    assert!(
        parsed["dead_confident"].is_number(),
        "health --json should have dead_confident"
    );
    assert!(
        parsed["dead_possible"].is_number(),
        "health --json should have dead_possible"
    );
    assert!(
        parsed["hotspots"].is_array(),
        "health --json should have hotspots array"
    );
    assert!(
        parsed["note_count"].is_number(),
        "health --json should have note_count"
    );
    assert!(
        parsed["note_warnings"].is_number(),
        "health --json should have note_warnings"
    );
    assert!(
        parsed["stats"]["schema_version"].is_number(),
        "health --json should have stats.schema_version"
    );
    assert!(
        parsed["stats"]["model_name"].is_string(),
        "health --json should have stats.model_name"
    );

    // Verify total_chunks > 0 (we indexed real files)
    let total_chunks = parsed["stats"]["total_chunks"]
        .as_u64()
        .expect("stats.total_chunks should be a number");
    assert!(
        total_chunks > 0,
        "total_chunks should be > 0 after indexing, got {}",
        total_chunks
    );
}

#[test]
#[serial]
fn test_health_cli_text() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    cqs()
        .args(["health"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Codebase Health"));
}

// =============================================================================
// Suggest command (TC-18)
// =============================================================================

#[test]
#[serial]
fn test_suggest_cli_json() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    let output = cqs()
        .args(["suggest", "--json"])
        .current_dir(dir.path())
        .assert()
        .success();

    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    // Output should be valid JSON — either an array of suggestions or an empty array
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("Invalid JSON: {} -- raw: {}", e, stdout));

    assert!(
        parsed.is_array(),
        "suggest --json should output a JSON array, got: {}",
        parsed
    );
}

// =============================================================================
// Deps command (TC-18)
// =============================================================================

#[test]
#[serial]
fn test_deps_cli_json() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    // Forward deps: who uses Config?
    let output = cqs()
        .args(["deps", "Config", "--json"])
        .current_dir(dir.path())
        .assert()
        .success();

    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("Invalid JSON: {} -- raw: {}", e, stdout));

    // Forward deps output is an array of chunk users
    assert!(
        parsed.is_array(),
        "deps --json (forward) should output a JSON array, got: {}",
        parsed
    );
}

#[test]
#[serial]
fn test_deps_reverse_cli_json() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    // Reverse deps: what types does validate use?
    let output = cqs()
        .args(["deps", "validate", "--reverse", "--json"])
        .current_dir(dir.path())
        .assert()
        .success();

    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("Invalid JSON: {} -- raw: {}", e, stdout));

    // Reverse deps output is an object with function, types, count
    assert!(
        parsed["function"].is_string(),
        "deps --reverse --json should have function field"
    );
    assert!(
        parsed["types"].is_array(),
        "deps --reverse --json should have types array"
    );
    assert!(
        parsed["count"].is_number(),
        "deps --reverse --json should have count field"
    );
}
