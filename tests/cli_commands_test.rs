//! Integration tests for P3-10 CLI commands: scout, where, related, impact-diff, stale
//!
//! Uses the same graph fixture as cli_graph_test.rs:
//!   src/lib.rs:  main() -> process(), process() -> validate(), process() -> transform()
//!   src/tests.rs: test_process() -> process()

use assert_cmd::Command;
use predicates::prelude::*;
use serial_test::serial;
use std::fs;
use std::process;
use tempfile::TempDir;

/// Get a Command for the cqs binary
fn cqs() -> Command {
    #[allow(deprecated)]
    Command::cargo_bin("cqs").expect("Failed to find cqs binary")
}

/// Create a project with call relationships for testing.
fn setup_graph_project() -> TempDir {
    let dir = TempDir::new().expect("Failed to create temp dir");
    let src = dir.path().join("src");
    fs::create_dir(&src).expect("Failed to create src dir");

    fs::write(
        src.join("lib.rs"),
        r#"
/// Entry point
pub fn main() {
    let data = process(42);
    println!("{}", data);
}

/// Process input through validation and transformation
pub fn process(input: i32) -> String {
    let valid = validate(input);
    if valid {
        transform(input)
    } else {
        String::from("invalid")
    }
}

/// Check if input is positive
fn validate(input: i32) -> bool {
    input > 0
}

/// Double and format the input
fn transform(input: i32) -> String {
    format!("result: {}", input * 2)
}
"#,
    )
    .expect("Failed to write lib.rs");

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

/// Initialize a git repo in the temp directory with an initial commit.
fn init_git_repo(dir: &TempDir) {
    let run = |args: &[&str]| {
        let status = process::Command::new("git")
            .args(args)
            .current_dir(dir.path())
            .stdout(process::Stdio::null())
            .stderr(process::Stdio::null())
            .status()
            .unwrap_or_else(|e| panic!("Failed to run git {:?}: {}", args, e));
        assert!(status.success(), "git {:?} failed", args);
    };
    run(&["init"]);
    run(&[
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@test.com",
        "add",
        ".",
    ]);
    run(&[
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@test.com",
        "commit",
        "-m",
        "init",
    ]);
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
// Scout command (P3-10)
// =============================================================================

#[test]
#[serial]
fn test_scout_json_output() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    let output = cqs()
        .args(["scout", "process data", "--json"])
        .current_dir(dir.path())
        .assert()
        .success();

    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("Invalid JSON: {} -- raw: {}", e, stdout));

    assert!(
        parsed["file_groups"].is_array(),
        "scout --json should have file_groups array"
    );
    assert!(
        parsed["summary"].is_object(),
        "scout --json should have summary object"
    );
}

#[test]
#[serial]
fn test_scout_text_output() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    cqs()
        .args(["scout", "validate input"])
        .current_dir(dir.path())
        .assert()
        .success();
    // Should at least not panic; output may vary
}

// =============================================================================
// Where command (P3-10)
// =============================================================================

#[test]
#[serial]
fn test_where_json_output() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    let output = cqs()
        .args(["where", "error handling function", "--json"])
        .current_dir(dir.path())
        .assert()
        .success();

    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("Invalid JSON: {} -- raw: {}", e, stdout));

    assert!(
        parsed["suggestions"].is_array(),
        "where --json should have suggestions array"
    );
}

#[test]
#[serial]
fn test_where_text_output() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    cqs()
        .args(["where", "validation helper"])
        .current_dir(dir.path())
        .assert()
        .success();
}

// =============================================================================
// Related command (P3-10)
// =============================================================================

#[test]
#[serial]
fn test_related_json_output() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    let output = cqs()
        .args(["related", "process", "--json"])
        .current_dir(dir.path())
        .assert()
        .success();

    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("Invalid JSON: {} -- raw: {}", e, stdout));

    assert!(
        parsed["target"].is_string(),
        "related --json should have target field"
    );
    assert!(
        parsed["shared_callers"].is_array(),
        "related --json should have shared_callers"
    );
    assert!(
        parsed["shared_callees"].is_array(),
        "related --json should have shared_callees"
    );
    assert!(
        parsed["shared_types"].is_array(),
        "related --json should have shared_types"
    );
}

#[test]
#[serial]
fn test_related_text_output() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    cqs()
        .args(["related", "validate"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("validate"));
}

#[test]
#[serial]
fn test_related_nonexistent_function() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    cqs()
        .args(["related", "nonexistent_fn_xyz"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("No function found"));
}

// =============================================================================
// Impact-diff command (P3-10)
// =============================================================================

#[test]
#[serial]
fn test_impact_diff_json_output() {
    let dir = setup_graph_project();
    init_git_repo(&dir);
    init_and_index(&dir);

    // Modify a file to create a diff (after git commit, so git diff shows changes)
    let lib_path = dir.path().join("src/lib.rs");
    let content = fs::read_to_string(&lib_path).unwrap();
    let modified = content.replace("input > 0", "input >= 0");
    fs::write(&lib_path, modified).unwrap();

    let output = cqs()
        .args(["impact-diff", "--json"])
        .current_dir(dir.path())
        .assert()
        .success();

    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("Invalid JSON: {} -- raw: {}", e, stdout));

    assert!(
        parsed["summary"].is_object(),
        "impact-diff --json should have summary object"
    );
}

#[test]
#[serial]
fn test_impact_diff_no_changes() {
    let dir = setup_graph_project();
    init_git_repo(&dir);
    init_and_index(&dir);

    // No modifications — should succeed with zero changes
    cqs()
        .args(["impact-diff", "--json"])
        .current_dir(dir.path())
        .assert()
        .success();
}

// =============================================================================
// Stale command (P3-10)
// =============================================================================

#[test]
#[serial]
fn test_stale_json_fresh_index() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    let output = cqs()
        .args(["stale", "--json"])
        .current_dir(dir.path())
        .assert()
        .success();

    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("Invalid JSON: {} -- raw: {}", e, stdout));

    assert!(
        parsed["stale"].is_array(),
        "stale --json should have stale array"
    );
    assert!(
        parsed["missing"].is_array(),
        "stale --json should have missing array"
    );
}

#[test]
#[serial]
fn test_stale_after_modification() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    // Wait briefly then modify a file
    std::thread::sleep(std::time::Duration::from_millis(100));
    let lib_path = dir.path().join("src/lib.rs");
    let content = fs::read_to_string(&lib_path).unwrap();
    fs::write(&lib_path, format!("{}\n// modified", content)).unwrap();

    let output = cqs()
        .args(["stale", "--json"])
        .current_dir(dir.path())
        .assert()
        .success();

    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("Invalid JSON: {} -- raw: {}", e, stdout));

    let stale = parsed["stale"].as_array().unwrap();
    assert!(!stale.is_empty(), "Modified file should appear as stale");
}

#[test]
#[serial]
fn test_stale_text_output() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    cqs()
        .args(["stale"])
        .current_dir(dir.path())
        .assert()
        .success();
}

#[test]
fn test_stale_no_index() {
    let dir = TempDir::new().expect("Failed to create temp dir");

    cqs()
        .args(["stale"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("not found").or(predicate::str::contains("Index")));
}

// =============================================================================
// Token budgeting (TC-5)
// =============================================================================

#[test]
#[serial]
fn test_query_tokens_limits_output() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    let output = cqs()
        .args(["--json", "--tokens", "500", "process data"])
        .current_dir(dir.path())
        .assert()
        .success();

    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("Invalid JSON: {} -- raw: {}", e, stdout));

    // When --tokens is specified, JSON output must have token_count and token_budget
    assert!(
        parsed["token_count"].is_number(),
        "query --tokens --json should have token_count field, got: {}",
        parsed
    );
    assert!(
        parsed["token_budget"].is_number(),
        "query --tokens --json should have token_budget field, got: {}",
        parsed
    );

    let token_count = parsed["token_count"].as_u64().unwrap();
    let token_budget = parsed["token_budget"].as_u64().unwrap();

    assert_eq!(
        token_budget, 500,
        "token_budget should match --tokens value"
    );
    assert!(
        token_count <= token_budget,
        "token_count ({}) should not exceed token_budget ({})",
        token_count,
        token_budget
    );
}

#[test]
#[serial]
fn test_gather_tokens_limits_output() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    let output = cqs()
        .args(["gather", "process data", "--tokens", "500", "--json"])
        .current_dir(dir.path())
        .assert()
        .success();

    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("Invalid JSON: {} -- raw: {}", e, stdout));

    assert!(
        parsed["token_count"].is_number(),
        "gather --tokens --json should have token_count field, got: {}",
        parsed
    );
    assert!(
        parsed["token_budget"].is_number(),
        "gather --tokens --json should have token_budget field, got: {}",
        parsed
    );

    let token_count = parsed["token_count"].as_u64().unwrap();
    let token_budget = parsed["token_budget"].as_u64().unwrap();

    assert_eq!(
        token_budget, 500,
        "token_budget should match --tokens value"
    );
    assert!(
        token_count <= token_budget,
        "token_count ({}) should not exceed token_budget ({})",
        token_count,
        token_budget
    );
}

// =============================================================================
// --ref flag integration tests (TC-6)
// =============================================================================

/// Create a reference source project with distinct content for --ref testing.
/// Content is about "database" operations — intentionally different from the
/// graph project's "process/validate/transform" theme so --ref results are
/// distinguishable from project results.
fn setup_ref_source() -> TempDir {
    let dir = TempDir::new().expect("Failed to create ref source dir");
    let src = dir.path().join("src");
    fs::create_dir(&src).expect("Failed to create src dir");

    fs::write(
        src.join("lib.rs"),
        r#"
/// Connect to the database with retry logic
pub fn connect_db(url: &str) -> Connection {
    let conn = Connection::new(url);
    conn.set_timeout(30);
    conn
}

/// Run a database migration to the target version
pub fn migrate(conn: &Connection, version: u32) {
    conn.execute("CREATE TABLE IF NOT EXISTS migrations (version INT)");
    conn.execute(&format!("INSERT INTO migrations VALUES ({})", version));
}

/// Query the database for user records
pub fn find_users(conn: &Connection, name: &str) -> Vec<User> {
    conn.query(&format!("SELECT * FROM users WHERE name = '{}'", name))
}
"#,
    )
    .expect("Failed to write ref source");

    dir
}

#[test]
#[serial]
fn test_query_with_ref() {
    let project = setup_graph_project();
    let ref_source = setup_ref_source();
    let xdg_home = TempDir::new().unwrap();

    init_and_index(&project);

    // Add reference with XDG_DATA_HOME redirected to temp dir
    cqs()
        .args(["ref", "add", "testref", ref_source.path().to_str().unwrap()])
        .env("XDG_DATA_HOME", xdg_home.path())
        .current_dir(project.path())
        .assert()
        .success();

    // Query with --ref should succeed and return results
    // Note: --ref is a top-level flag, QUERY is a top-level positional
    let output = cqs()
        .args(["database connection", "--ref", "testref", "--json"])
        .env("XDG_DATA_HOME", xdg_home.path())
        .current_dir(project.path())
        .assert()
        .success();

    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("Invalid JSON: {} -- raw: {}", e, stdout));

    assert!(
        parsed["results"].is_array(),
        "query --ref --json should have results array"
    );
    let results = parsed["results"].as_array().unwrap();
    assert!(
        !results.is_empty(),
        "query --ref should return at least one result from reference"
    );
}

#[test]
#[serial]
fn test_gather_with_ref() {
    let project = setup_graph_project();
    let ref_source = setup_ref_source();
    let xdg_home = TempDir::new().unwrap();

    init_and_index(&project);

    cqs()
        .args(["ref", "add", "testref", ref_source.path().to_str().unwrap()])
        .env("XDG_DATA_HOME", xdg_home.path())
        .current_dir(project.path())
        .assert()
        .success();

    // gather --ref seeds from the reference index
    let output = cqs()
        .args(["gather", "database migration", "--ref", "testref", "--json"])
        .env("XDG_DATA_HOME", xdg_home.path())
        .current_dir(project.path())
        .assert()
        .success();

    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("Invalid JSON: {} -- raw: {}", e, stdout));

    assert!(
        parsed["chunks"].is_array(),
        "gather --ref --json should have chunks array"
    );
}

#[test]
#[serial]
fn test_query_ref_not_found() {
    let project = setup_graph_project();
    let xdg_home = TempDir::new().unwrap();
    init_and_index(&project);

    // Query with --ref pointing to nonexistent reference
    // Note: --ref is a top-level flag, QUERY is a top-level positional
    cqs()
        .args(["anything", "--ref", "nonexistent", "--json"])
        .env("XDG_DATA_HOME", xdg_home.path())
        .current_dir(project.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("not found"));
}

#[test]
#[serial]
fn test_ref_list_shows_added_ref() {
    let project = setup_graph_project();
    let ref_source = setup_ref_source();
    let xdg_home = TempDir::new().unwrap();

    init_and_index(&project);

    cqs()
        .args(["ref", "add", "myref", ref_source.path().to_str().unwrap()])
        .env("XDG_DATA_HOME", xdg_home.path())
        .current_dir(project.path())
        .assert()
        .success();

    // ref list should show the added reference
    cqs()
        .args(["ref", "list"])
        .env("XDG_DATA_HOME", xdg_home.path())
        .current_dir(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("myref"));
}
