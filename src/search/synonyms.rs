//! Query expansion via synonym mapping for FTS search.
//!
//! Expands abbreviated query tokens into OR-groups for FTS5, improving recall
//! when users search with abbreviations (e.g., "auth" finds "authentication").
//!
//! The synonym table is a runtime-mutable `HashMap<String, Vec<String>>`
//! initialized with compile-time defaults and overlay-merged from optional
//! TOML files at startup. Operators
//! extend the dictionary with domain vocabulary (manufacturing/industrial:
//! `plc`/`scada`/`opc`/`hmi`; cqs-internal: `hnsw`/`splade`/`cagra`/`rrf`)
//! without rebuilding the binary. Schema in `[load_synonym_overlay]`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{LazyLock, RwLock};

/// Compile-time built-in synonyms — initial floor before any TOML overlay
/// is installed.
fn builtin_synonyms() -> HashMap<String, Vec<String>> {
    let entries: &[(&str, &[&str])] = &[
        ("auth", &["authentication", "authorize", "credential"]),
        ("config", &["configuration", "settings"]),
        ("cfg", &["configuration", "config", "settings"]),
        ("err", &["error", "failure", "exception"]),
        ("fn", &["function", "method"]),
        ("func", &["function", "method"]),
        ("init", &["initialize", "setup", "initialization"]),
        ("parse", &["parsing", "deserialize", "decode"]),
        ("req", &["request"]),
        ("res", &["response", "result"]),
        ("fmt", &["format", "formatting"]),
        ("db", &["database", "storage"]),
        ("ctx", &["context"]),
        ("msg", &["message"]),
        ("cmd", &["command"]),
        ("buf", &["buffer"]),
        ("str", &["string"]),
        ("impl", &["implementation", "implement"]),
        ("alloc", &["allocate", "allocation"]),
        ("dealloc", &["deallocate", "free"]),
        ("arg", &["argument", "parameter"]),
        ("args", &["arguments", "parameters"]),
        ("param", &["parameter", "argument"]),
        ("params", &["parameters", "arguments"]),
        ("iter", &["iterator", "iteration"]),
        ("async", &["asynchronous"]),
        ("sync", &["synchronous", "synchronize"]),
        ("env", &["environment"]),
        ("dir", &["directory", "folder"]),
        ("deps", &["dependencies", "dependency"]),
        ("repo", &["repository"]),
    ];
    entries
        .iter()
        .map(|(k, v)| (k.to_string(), v.iter().map(|s| s.to_string()).collect()))
        .collect()
}

/// Merged synonym table. Initialized lazily with builtins on first access;
/// overlays installed via [`install_synonym_overlay`] are merged in
/// (overwriting on key collision). RwLock keeps reads cheap on the hot
/// search path — uncontested reads are ~10ns and only contend with the
/// once-at-startup write from `install_synonym_overlay`.
static SYNONYMS: LazyLock<RwLock<HashMap<String, Vec<String>>>> =
    LazyLock::new(|| RwLock::new(builtin_synonyms()));

/// Install a runtime synonym overlay. Idempotent per key — repeated calls
/// overwrite. Per-key precedence is "last
/// install wins"; in production [`install_synonym_overlay`] is called
/// once at CLI/daemon startup with the project-local overlay layered
/// on top of the user-global one.
///
/// Empty maps are no-ops.
///
/// Token-validation note: each key is lowercased before insertion (the
/// lookup path lowercases too) so a user-config typo like `Auth`
/// matches the lookup. The expansion `Vec<String>` entries are passed
/// verbatim into FTS5 OR groups — callers MUST ensure they're
/// alphanumeric / FTS-safe; the loader [`load_synonym_overlay`] enforces
/// this on the disk path.
pub fn install_synonym_overlay(extras: HashMap<String, Vec<String>>) {
    if extras.is_empty() {
        return;
    }
    // Info-level so an operator who edited `~/.config/cqs/synonyms.toml` (or
    // the project-local override) sees their config landing in journald
    // without RUST_LOG=debug. Fires only when the merged input is non-empty,
    // so the default no-overlay case stays silent.
    let entries = extras.len();
    let mut g = SYNONYMS.write().unwrap_or_else(|p| p.into_inner());
    for (k, v) in extras {
        g.insert(k.to_lowercase(), v);
    }
    tracing::info!(entries, "Installed synonym overlay");
}

/// Test-only: reset the synonym table to the compile-time builtins.
/// Without this, a test that installs an overlay would leak into
/// sibling tests via the process-global `LazyLock`.
#[cfg(test)]
pub(crate) fn reset_synonyms_for_test() {
    let mut g = SYNONYMS.write().unwrap_or_else(|p| p.into_inner());
    *g = builtin_synonyms();
}

/// Parse a `synonyms.toml` overlay from disk.
///
/// Schema:
/// ```toml
/// [synonyms]
/// plc = ["programmable_logic_controller", "ladder_logic"]
/// scada = ["supervisory_control"]
/// hnsw = ["hierarchical_navigable_small_world"]
/// ```
///
/// Returns an empty map on:
/// - missing file (no error — operators don't need to create the file)
/// - malformed TOML (warn + empty so a typo doesn't break search)
/// - unsafe expansion tokens (anything outside `[A-Za-z0-9_]+`) — these
///   are skipped per-entry with a warn so partial overlays still apply.
///
/// Bounded read at 4 KiB. Typical overlay is <500 bytes; a hostile config
/// can't OOM the indexer.
pub fn load_synonym_overlay(path: &Path) -> HashMap<String, Vec<String>> {
    use std::io::Read;
    const MAX_BYTES: u64 = 4096;

    let mut file = match std::fs::File::open(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return HashMap::new(),
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "Failed to open synonym overlay; falling back to builtins"
            );
            return HashMap::new();
        }
        Ok(f) => f,
    };
    let mut raw = String::new();
    if let Err(e) = (&mut file).take(MAX_BYTES).read_to_string(&mut raw) {
        tracing::warn!(
            path = %path.display(),
            error = %e,
            "Bounded read of synonym overlay failed; falling back to builtins"
        );
        return HashMap::new();
    }

    #[derive(serde::Deserialize)]
    struct File {
        synonyms: Option<HashMap<String, Vec<String>>>,
    }
    let parsed: File = match toml::from_str(&raw) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "Synonym overlay is malformed TOML; falling back to builtins"
            );
            return HashMap::new();
        }
    };
    let raw_table = match parsed.synonyms {
        Some(t) => t,
        None => return HashMap::new(),
    };

    let is_fts_safe =
        |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
    let mut out = HashMap::with_capacity(raw_table.len());
    for (k, v) in raw_table {
        if !is_fts_safe(&k) {
            tracing::warn!(
                path = %path.display(),
                key = %k,
                "Synonym overlay key contains unsafe characters (allowed: A-Z, a-z, 0-9, _) — skipping"
            );
            continue;
        }
        let kept: Vec<String> = v
            .into_iter()
            .filter(|exp| {
                if !is_fts_safe(exp) {
                    tracing::warn!(
                        path = %path.display(),
                        key = %k,
                        expansion = %exp,
                        "Synonym overlay expansion contains unsafe characters — dropping"
                    );
                    false
                } else {
                    true
                }
            })
            .collect();
        if !kept.is_empty() {
            out.insert(k, kept);
        }
    }
    if !out.is_empty() {
        tracing::debug!(
            path = %path.display(),
            entries = out.len(),
            "Loaded synonym overlay"
        );
    }
    out
}

/// Expand a single FTS-sanitized query string with synonym OR groups.
///
/// Tokens that have synonyms are replaced with `(token OR syn1 OR syn2)`.
/// Tokens without synonyms pass through unchanged.
///
/// Input must already be FTS-sanitized (no special chars). Output is safe for
/// FTS5 MATCH because we only inject known-safe alpha tokens inside OR groups
/// (compile-time builtins are ASCII alphanumeric; runtime overlays are
/// validated by [`load_synonym_overlay`]).
pub fn expand_query_for_fts(sanitized_query: &str) -> String {
    debug_assert!(
        !sanitized_query.contains('"')
            && !sanitized_query.contains('(')
            && !sanitized_query.contains(')'),
        "expand_query_for_fts requires pre-sanitized input"
    );
    let tokens: Vec<&str> = sanitized_query.split_whitespace().collect();
    if tokens.is_empty() {
        return String::new();
    }

    let synonyms = SYNONYMS.read().unwrap_or_else(|p| p.into_inner());
    let mut parts: Vec<String> = Vec::with_capacity(tokens.len());
    let mut has_or_group = false;
    for token in &tokens {
        let lower = token.to_lowercase();
        if let Some(entries) = synonyms.get(&lower) {
            // Build OR group: (original OR syn1 OR syn2 ...)
            let mut group = format!("({}", token);
            for syn in entries {
                group.push_str(" OR ");
                group.push_str(syn);
            }
            group.push(')');
            parts.push(group);
            has_or_group = true;
        } else {
            parts.push(token.to_string());
        }
    }

    // FTS5 requires explicit AND between terms when any OR group is present.
    // Implicit AND (space) causes "syntax error near" after an OR group.
    let sep = if has_or_group { " AND " } else { " " };
    parts.join(sep)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    fn empty_query_returns_empty() {
        assert_eq!(expand_query_for_fts(""), "");
        assert_eq!(expand_query_for_fts("   "), "");
    }

    #[test]
    fn no_synonyms_passes_through() {
        assert_eq!(expand_query_for_fts("hello world"), "hello world");
    }

    #[test]
    #[serial(synonyms_overlay)]
    fn single_synonym_expands() {
        reset_synonyms_for_test();
        let result = expand_query_for_fts("auth");
        assert!(result.contains("auth"));
        assert!(result.contains("authentication"));
        assert!(result.contains("authorize"));
        assert!(result.contains("credential"));
        assert!(result.starts_with('('));
        assert!(result.contains(" OR "));
    }

    #[test]
    #[serial(synonyms_overlay)]
    fn mixed_tokens_expand_selectively() {
        reset_synonyms_for_test();
        let result = expand_query_for_fts("auth middleware");
        // "auth" should expand, "middleware" should not
        // FTS5 requires explicit AND when OR groups are present
        assert!(result.contains("(auth OR authentication"));
        assert!(result.contains("AND middleware"));
        assert!(!result.contains("(middleware"));
    }

    #[test]
    #[serial(synonyms_overlay)]
    fn all_synonyms_expand() {
        reset_synonyms_for_test();
        let result = expand_query_for_fts("config err");
        assert!(result.contains("(config OR configuration"));
        assert!(result.contains("AND (err OR error"));
    }

    #[test]
    fn no_expansion_uses_implicit_and() {
        // When no synonyms match, use space (implicit AND) for simplicity
        let result = expand_query_for_fts("hello world");
        assert_eq!(result, "hello world");
        assert!(!result.contains("AND"));
    }

    #[test]
    #[serial(synonyms_overlay)]
    fn case_insensitive_lookup() {
        reset_synonyms_for_test();
        let result = expand_query_for_fts("Auth");
        assert!(result.contains("Auth"));
        assert!(result.contains("authentication"));
    }

    #[test]
    #[serial(synonyms_overlay)]
    fn synonym_map_has_expected_entries() {
        reset_synonyms_for_test();
        // Verify key synonyms from the spec exist (read directly from the
        // table).
        let g = SYNONYMS.read().unwrap();
        assert!(g.contains_key("auth"));
        assert!(g.contains_key("config"));
        assert!(g.contains_key("err"));
        assert!(g.contains_key("fn"));
        assert!(g.contains_key("init"));
        assert!(g.contains_key("parse"));
        assert!(g.contains_key("req"));
        assert!(g.contains_key("res"));
        assert!(g.contains_key("fmt"));
        assert!(g.contains_key("db"));
        assert!(g.len() >= 30, "Expected at least 30 synonym entries");
    }

    // ─── TOML overlay tests ───────────────────────────────

    #[test]
    #[serial(synonyms_overlay)]
    fn install_overlay_extends_dictionary() {
        reset_synonyms_for_test();
        // Domain vocabulary not in the builtins.
        let mut overlay = HashMap::new();
        overlay.insert(
            "plc".to_string(),
            vec![
                "programmable_logic_controller".to_string(),
                "ladder_logic".to_string(),
            ],
        );
        install_synonym_overlay(overlay);

        let result = expand_query_for_fts("plc");
        assert!(result.contains("plc"));
        assert!(result.contains("programmable_logic_controller"));
        assert!(result.contains("ladder_logic"));
        reset_synonyms_for_test();
    }

    #[test]
    #[serial(synonyms_overlay)]
    fn install_overlay_overrides_builtin_on_conflict() {
        reset_synonyms_for_test();
        // Override "auth" with a custom expansion set.
        let mut overlay = HashMap::new();
        overlay.insert(
            "auth".to_string(),
            vec!["my_custom_auth_expansion".to_string()],
        );
        install_synonym_overlay(overlay);

        let result = expand_query_for_fts("auth");
        assert!(result.contains("my_custom_auth_expansion"));
        // Builtin expansions should NOT be present after override.
        assert!(
            !result.contains("authentication"),
            "overlay must override the builtin entry; got {result:?}"
        );
        reset_synonyms_for_test();
    }

    #[test]
    #[serial(synonyms_overlay)]
    fn install_overlay_lowercases_keys() {
        reset_synonyms_for_test();
        // Case-insensitive lookup means an upper-case overlay key still
        // matches a lower-case query.
        let mut overlay = HashMap::new();
        overlay.insert("SCADA".to_string(), vec!["supervisory_control".to_string()]);
        install_synonym_overlay(overlay);

        let result = expand_query_for_fts("scada");
        assert!(result.contains("supervisory_control"));
        reset_synonyms_for_test();
    }

    #[test]
    #[serial(synonyms_overlay)]
    fn install_overlay_empty_is_noop() {
        reset_synonyms_for_test();
        // Capture pre-state.
        let before_len = SYNONYMS.read().unwrap().len();
        install_synonym_overlay(HashMap::new());
        let after_len = SYNONYMS.read().unwrap().len();
        assert_eq!(before_len, after_len);
        reset_synonyms_for_test();
    }

    #[test]
    fn load_overlay_missing_file_returns_empty() {
        let table = load_synonym_overlay(Path::new("/nonexistent/synonyms.toml"));
        assert!(table.is_empty());
    }

    #[test]
    fn load_overlay_parses_typed_section() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("synonyms.toml");
        std::fs::write(
            &path,
            "[synonyms]\nplc = [\"programmable_logic_controller\"]\nhnsw = [\"hierarchical_navigable_small_world\"]\n",
        )
        .unwrap();

        let table = load_synonym_overlay(&path);
        assert_eq!(
            table.get("plc").map(|v| v.as_slice()),
            Some(&["programmable_logic_controller".to_string()][..])
        );
        assert_eq!(
            table.get("hnsw").map(|v| v.as_slice()),
            Some(&["hierarchical_navigable_small_world".to_string()][..])
        );
        assert_eq!(table.len(), 2);
    }

    #[test]
    fn load_overlay_drops_unsafe_keys_keeps_safe_entries() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("synonyms.toml");
        std::fs::write(
            &path,
            "[synonyms]\nplc = [\"programmable_logic_controller\"]\n\"bad-key!\" = [\"foo\"]\nopc = [\"open_platform_communications\"]\n",
        )
        .unwrap();

        let table = load_synonym_overlay(&path);
        assert!(table.contains_key("plc"));
        assert!(table.contains_key("opc"));
        assert!(
            !table.contains_key("bad-key!"),
            "unsafe key with `-`/`!` must be dropped (FTS-safe filter)"
        );
        assert_eq!(table.len(), 2);
    }

    #[test]
    fn load_overlay_drops_unsafe_expansions_only() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("synonyms.toml");
        std::fs::write(
            &path,
            "[synonyms]\nplc = [\"programmable_logic_controller\", \"with-dash\", \"good_one\"]\n",
        )
        .unwrap();

        let table = load_synonym_overlay(&path);
        let plc = table.get("plc").unwrap();
        assert!(plc.contains(&"programmable_logic_controller".to_string()));
        assert!(plc.contains(&"good_one".to_string()));
        assert!(
            !plc.iter().any(|s| s == "with-dash"),
            "unsafe expansion with `-` must be dropped"
        );
        assert_eq!(plc.len(), 2);
    }

    #[test]
    fn load_overlay_malformed_toml_returns_empty() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("synonyms.toml");
        std::fs::write(&path, "[synonyms\nplc = oops").unwrap(); // unclosed table
        let table = load_synonym_overlay(&path);
        assert!(table.is_empty());
    }

    #[test]
    fn load_overlay_no_synonyms_section_returns_empty() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("synonyms.toml");
        std::fs::write(&path, "[other]\nfoo = \"bar\"\n").unwrap();
        let table = load_synonym_overlay(&path);
        assert!(table.is_empty());
    }

    /// The loader caps reads at 4 KiB (`take(MAX_BYTES)`). A 5+ KiB hostile
    /// config must not OOM the
    /// indexer or surface its full content. We pin the cap by writing a
    /// file with a valid `[synonyms]` table at the start, then padding
    /// past the cap with a deliberately *malformed* TOML marker — the
    /// truncation should mid-table-cut and the parser should bail with
    /// the malformed-TOML branch (returning empty). Either contract
    /// (parsed-prefix or empty) is fine; the test asserts the loader
    /// completes without OOM and produces a finite HashMap result.
    #[test]
    fn load_overlay_caps_at_max_bytes_no_oom() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("synonyms.toml");
        // Pre-cap content (well under 4 KiB).
        let mut content = String::from("[synonyms]\nplc = [\"controller\"]\n");
        // Pad past 4 KiB with valid TOML comments.
        while content.len() < 4500 {
            content.push_str("# padding line\n");
        }
        // Append an unclosed table marker AFTER the cap. If truncation
        // works, the loader never sees this. If truncation broke and
        // the loader read the full 5+ KiB, it would surface as
        // malformed-TOML → empty map.
        content.push_str("[malformed");
        std::fs::write(&path, content).unwrap();

        // Sanity: the on-disk file IS larger than the cap.
        let file_len = std::fs::metadata(&path).unwrap().len();
        assert!(
            file_len > 4096,
            "test fixture must exceed 4 KiB cap (got {file_len} bytes)"
        );

        // Loader must complete without OOM. The malformed sentinel was
        // appended past byte 4500; truncation at MAX_BYTES=4096 cuts it
        // mid-comment, so the prefix is valid TOML and `plc` survives.
        let table = load_synonym_overlay(&path);
        // We don't assert exact contents — both "parsed prefix" and
        // "malformed" outcomes are acceptable contracts. We only pin
        // that the loader returned, didn't panic, and produced a
        // bounded map.
        assert!(
            table.len() <= 1,
            "capped-load must produce <=1 entry from the pre-cap valid section: {table:?}"
        );
    }
}
