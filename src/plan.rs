//! Task planning with template classification.
//!
//! Classifies a task description into one of 11 task-type templates,
//! runs scout for relevant code, and produces an implementation checklist.

use std::path::Path;

use serde::Serialize;

use crate::scout::{scout, ScoutResult};
use crate::{Embedder, Store};

/// A task-type template for implementation planning.
#[derive(Debug, Clone, Serialize)]
pub struct TaskTemplate {
    pub name: &'static str,
    pub description: &'static str,
    pub checklist: &'static [&'static str],
    pub patterns: &'static [&'static str],
}

/// Result of a plan operation.
#[derive(Debug, Clone, Serialize)]
pub struct PlanResult {
    pub template: String,
    pub template_description: String,
    pub checklist: Vec<String>,
    pub patterns: Vec<String>,
    pub scout: ScoutResult,
}

// ---------------------------------------------------------------------------
// Template definitions
// ---------------------------------------------------------------------------

struct TemplateEntry {
    template: TaskTemplate,
    keywords: &'static [(&'static str, f32)],
}

const TEMPLATES: &[TemplateEntry] = &[
    TemplateEntry {
        template: TaskTemplate {
            name: "Add/Replace a CLI Flag",
            description: "Adding a new flag, renaming a flag, changing a flag's type",
            checklist: &[
                "src/cli/mod.rs — Commands enum variant: add/modify #[arg] field",
                "src/cli/mod.rs — run_with() match arm: update destructuring",
                "src/cli/commands/<name>.rs — cmd_<name>() signature and branching logic",
                "src/cli/commands/<name>.rs — Display functions if flag affects output",
                "src/store/*.rs / src/lib.rs — Only if flag affects query behavior",
                "tests/<name>_test.rs — Add case for new value",
                ".claude/skills/cqs/SKILL.md — Update argument-hint and usage",
                "README.md — Update examples if command is featured",
            ],
            patterns: &[
                "Output format flags: #[arg(long, value_enum, default_value_t)]",
                "Display functions: display_<name>_text(), display_<name>_json()",
                "JSON output: serde_json::to_string_pretty on #[derive(Serialize)] structs",
            ],
        },
        keywords: &[
            ("flag", 2.0), ("arg", 1.5), ("--", 2.0), ("clap", 1.5),
            ("option", 1.0), ("parameter", 0.5),
        ],
    },
    TemplateEntry {
        template: TaskTemplate {
            name: "Add a New CLI Command",
            description: "Adding an entirely new cqs <command>",
            checklist: &[
                "src/cli/mod.rs — Add variant to Commands enum with args",
                "src/cli/mod.rs — Add match arm in run_with()",
                "src/cli/commands/<name>.rs — New file: cmd_<name>() following command pattern",
                "src/cli/commands/mod.rs — Add mod + pub(crate) use",
                "src/lib.rs or src/<module>.rs — Library function if logic is non-trivial",
                "tests/<name>_test.rs — Integration tests",
                ".claude/skills/cqs/SKILL.md — Add to commands list",
                "CLAUDE.md — Add to key commands list",
                "README.md — Add to command reference",
                "CONTRIBUTING.md — Update Architecture Overview",
            ],
            patterns: &[
                "Command files are ~50-150 lines: store/library calls, then display",
                "Boilerplate: find_project_root() + resolve_index_dir() + Store::open()",
                "JSON output with --json flag, text output respects --quiet",
                "Tracing span at entry: let _span = tracing::info_span!(\"cmd_<name>\").entered()",
            ],
        },
        keywords: &[
            ("new command", 3.0), ("add command", 3.0), ("subcommand", 2.0),
            ("command", 1.0), ("cli command", 2.0),
        ],
    },
    TemplateEntry {
        template: TaskTemplate {
            name: "Fix a Bug",
            description: "Something produces wrong results, panics, or misbehaves",
            checklist: &[
                "Reproduce: understand exact failure (input → actual → expected)",
                "Locate: cqs scout to find relevant code",
                "Trace callers: cqs callers <function> — who calls the buggy code?",
                "Check tests: cqs test-map <function> — do tests cover the failing case?",
                "Fix: minimal change in the library layer, not the CLI layer",
                "Add test: regression test that would have caught this bug",
                "Check impact: cqs impact <function> — did the fix change behavior for other callers?",
            ],
            patterns: &[
                "Fix in src/*.rs (library), test in tests/*.rs or inline #[cfg(test)]",
                "Use tracing::warn! for recoverable errors, bail! for unrecoverable",
                "Never .unwrap() in library code — use ? or match + tracing::warn!",
            ],
        },
        keywords: &[
            ("bug", 2.0), ("fix", 1.5), ("broken", 2.0), ("wrong", 1.5),
            ("crash", 2.0), ("panic", 2.0), ("error", 0.5), ("fail", 1.0),
            ("incorrect", 1.5), ("regression", 1.5),
        ],
    },
    TemplateEntry {
        template: TaskTemplate {
            name: "Add Language Support",
            description: "Adding a new programming language to the parser",
            checklist: &[
                "Cargo.toml — Add tree-sitter grammar dependency (optional)",
                "Cargo.toml features — Add lang-<name> feature, add to default and lang-all",
                "src/language/mod.rs — Add to define_languages! macro invocation",
                "src/language/<lang>.rs — New file: LanguageDef with chunk_query, call_query, extensions",
                "tests/fixtures/sample.<ext> — Sample file for parser tests",
                "tests/parser_test.rs — Parser tests for the new language",
            ],
            patterns: &[
                "One-liner in define_languages! handles registration",
                "Chunk query captures must use names from capture_types: function, struct, class, enum, trait, interface, const",
                "Call query uses @callee capture",
                "Look at similar languages for query patterns (e.g., Ruby for dynamic, Haskell for functional)",
            ],
        },
        keywords: &[
            ("language", 2.0), ("parser", 1.5), ("lang-", 1.5),
            ("language support", 3.0), ("add language", 3.0),
        ],
    },
    TemplateEntry {
        template: TaskTemplate {
            name: "Add ChunkType Variant",
            description: "Adding a new chunk type (e.g., Extension, Protocol)",
            checklist: &[
                "src/language/mod.rs — Add one line to define_chunk_types! macro (Display, FromStr auto-generated)",
                "src/language/mod.rs — Update is_callable() and human_name() if needed",
                "src/language/mod.rs — Add capture name mapping in capture_name_to_chunk_type()",
                "src/nl.rs — Add natural language label for the variant",
                "src/language/<lang>.rs — Add capture using the new variant name in chunk_query",
                "tests/parser_test.rs — Parser tests for each language using the variant",
                "ROADMAP.md — Update ChunkType Variant Status table",
            ],
            patterns: &[
                "is_callable() returns true for Function, Method, Macro — most others false",
                "define_chunk_types! generates Display (lowercase), FromStr (snake_case + spaces), all_names()",
                "capture_name_to_chunk_type() maps tree-sitter capture names to ChunkType (may differ from Display)",
                "Container extraction uses capture_types to decide container vs leaf",
            ],
        },
        keywords: &[
            ("chunk type", 3.0), ("ChunkType", 3.0), ("variant", 1.5),
            ("chunk variant", 3.0),
        ],
    },
    TemplateEntry {
        template: TaskTemplate {
            name: "Add Injection Rule",
            description: "Adding multi-grammar parsing (e.g., HTML→JS, PHP→HTML)",
            checklist: &[
                "src/language/<host>.rs — Add InjectionRule to LanguageDef::injection_rules()",
                "src/language/<target>.rs — Ensure target LanguageDef exists and parses in isolation",
                "src/parser/injection.rs — Only if new detection logic needed",
                "tests/fixtures/sample.<ext> — Sample file with embedded content",
                "tests/parser_test.rs — Verify chunks from both host and injected language",
                "ROADMAP.md — Update Multi-Grammar Parsing section",
            ],
            patterns: &[
                "content_scoped_lines prevents container-spans-file problem",
                "detect_language callbacks inspect attributes (e.g., lang=\"ts\")",
                "set_included_ranges() for byte-range isolation",
                "Recursive injections must respect depth limit (default 3)",
            ],
        },
        keywords: &[
            ("injection", 2.5), ("embedded", 1.5), ("multi-grammar", 3.0),
            ("inject", 2.0), ("injection rule", 3.0),
        ],
    },
    TemplateEntry {
        template: TaskTemplate {
            name: "Performance Optimization",
            description: "Improving speed or reducing resource usage",
            checklist: &[
                "Benchmark before: cargo bench or manual timing. Record baseline",
                "Profile: cqs scout to find hot path, cqs callers to trace call chain",
                "Identify approach: lazy loading, caching, reduced allocations, parallelism",
                "Implement: minimal change, prefer data structure changes over algorithmic rewrites",
                "Benchmark after: same benchmark as before. Quantify improvement",
                "Regression test: same inputs produce same outputs",
                "Check callers: cqs impact — did the optimization change the API surface?",
            ],
            patterns: &[
                "HNSW candidate fetch: load only (id, embedding) for scoring, full content for top-k",
                "Rayon par_iter for embarrassingly parallel work — check for shared mutable state",
                "tracing::info_span! around hot paths for flame graph visibility",
            ],
        },
        keywords: &[
            ("performance", 2.0), ("speed", 1.5), ("slow", 2.0),
            ("memory", 1.0), ("optimize", 2.0), ("perf", 1.5), ("fast", 1.0),
            ("benchmark", 1.5), ("latency", 1.5),
        ],
    },
    TemplateEntry {
        template: TaskTemplate {
            name: "Audit Finding Fix",
            description: "Fixing an issue identified during a code audit",
            checklist: &[
                "Read triage entry: priority, category, description from docs/audit-triage.md",
                "Locate: cqs scout — verify the issue still exists",
                "Assess scope: cqs impact — how many callers are affected?",
                "Fix: follow the triage entry's suggested approach",
                "Add test: cover the specific scenario from the finding",
                "Update triage: mark entry as fixed with PR reference",
                "Check related findings: same category may have related issues",
            ],
            patterns: &[
                "P1 findings: fix immediately, standalone PR",
                "P2-P3: batch by category into single PR",
                "P4: fix opportunistically when touching nearby code",
            ],
        },
        keywords: &[
            ("audit", 2.5), ("finding", 2.0), ("triage", 2.0),
            ("P1", 2.0), ("P2", 2.0), ("P3", 1.5), ("P4", 1.0),
        ],
    },
    TemplateEntry {
        template: TaskTemplate {
            name: "Add Tree-Sitter Grammar",
            description: "Adding a new tree-sitter grammar dependency",
            checklist: &[
                "Cargo.toml — Add grammar crate as optional dependency (prefer crates.io, git dep with rev pin if unpublished)",
                "src/language/<lang>.rs — Wire grammar via tree_sitter_<lang>::LANGUAGE or language()",
                "Cargo.toml features — Add to lang-<name> feature gate, default, and lang-all",
                "Verify compatibility: grammar must target tree-sitter >=0.24, <0.27",
                "Tests: cargo test --features lang-<name>",
                "If forked: document fork reason in Cargo.toml comment",
            ],
            patterns: &[
                "Git deps need rev pin, not branch — branches break reproducibility",
                "Some grammars export LANGUAGE (static), others language() (function)",
                "Monolithic grammars (Razor, VB.NET) don't need injection",
            ],
        },
        keywords: &[
            ("grammar", 2.0), ("tree-sitter", 2.5), ("tree_sitter", 2.5),
        ],
    },
    TemplateEntry {
        template: TaskTemplate {
            name: "Schema Migration",
            description: "Bumping the SQLite schema version",
            checklist: &[
                "src/store/helpers.rs — Bump CURRENT_SCHEMA_VERSION",
                "src/store/migrations.rs — Add migrate_vN_to_vM() function",
                "src/store/migrations.rs — Register in run_migration() match arms",
                "src/schema.sql — Update schema comment and add column/table",
                "src/store/*.rs — Update queries that read/write affected tables",
                "tests/store_test.rs — Update schema version assertion",
                "src/store/migrations.rs — Add migration test",
                "PROJECT_CONTINUITY.md — Update schema version in Architecture section",
            ],
            patterns: &[
                "Migrations must be idempotent: IF NOT EXISTS guards",
                "For new columns with NOT NULL, use DEFAULT or populate from existing data",
                "We use a metadata table for schema_version, not PRAGMA user_version",
                "Test with real old-version database if available",
            ],
        },
        keywords: &[
            ("schema", 2.5), ("migration", 2.5), ("column", 1.5),
            ("table", 1.0), ("ALTER", 2.0), ("schema version", 3.0),
        ],
    },
    TemplateEntry {
        template: TaskTemplate {
            name: "Refactor / Extract",
            description: "Moving code, splitting files, extracting shared helpers",
            checklist: &[
                "Find all call sites: cqs callers <function> for each function being moved",
                "Check similar code: cqs similar <function> to find duplicates to consolidate",
                "Plan visibility: pub(crate) for cross-module, pub for public API, private for same-module",
                "Move tests with code: #[cfg(test)] mod tests works in submodules",
                "Update imports: each file needs its own use statements",
                "Verify callers compile: all callers must update their use paths",
                "CONTRIBUTING.md — Update Architecture Overview for structural changes",
            ],
            patterns: &[
                "impl Foo blocks can live in separate files (Rust allows multiple)",
                "Trait method imports don't carry over to submodule files",
                "Use pub(crate) for types/constants shared across submodules",
            ],
        },
        keywords: &[
            ("refactor", 2.5), ("extract", 2.0), ("move", 1.0),
            ("split", 2.0), ("rename", 1.5), ("reorganize", 2.0),
        ],
    },
];

// ---------------------------------------------------------------------------
// Classification
// ---------------------------------------------------------------------------

/// Classify a task description into the best-matching template.
///
/// Uses keyword scoring. Returns the template index (into TEMPLATES).
/// Falls back to "Fix a Bug" (index 2) if no keywords match.
fn classify(description: &str) -> usize {
    let lower = description.to_lowercase();
    let mut best_idx = TEMPLATES
        .iter()
        .position(|e| e.template.name == "Fix a Bug")
        .unwrap_or(0);
    let mut best_score = 0.0f32;

    for (i, entry) in TEMPLATES.iter().enumerate() {
        let mut score = 0.0f32;
        for &(keyword, weight) in entry.keywords {
            if lower.contains(keyword) {
                score += weight;
            }
        }
        if score > best_score {
            best_score = score;
            best_idx = i;
        }
    }

    best_idx
}

/// Retrieves a task template by index.
///
/// # Arguments
///
/// * `idx` - The index of the template to retrieve from the templates collection.
///
/// # Returns
///
/// A reference to the `TaskTemplate` at the specified index.
///
/// # Panics
///
/// Panics if `idx` is out of bounds for the `TEMPLATES` array.
pub fn get_template(idx: usize) -> &'static TaskTemplate {
    &TEMPLATES[idx].template
}

/// List all available template names.
pub fn template_names() -> Vec<&'static str> {
    TEMPLATES.iter().map(|t| t.template.name).collect()
}

// ---------------------------------------------------------------------------
// Plan
// ---------------------------------------------------------------------------

/// Generate an implementation plan for a task.
///
/// Classifies the task, runs scout, and returns a structured plan
/// combining the template checklist with scout results.
pub fn plan(
    store: &Store,
    embedder: &Embedder,
    description: &str,
    root: &Path,
    limit: usize,
) -> Result<PlanResult, crate::AnalysisError> {
    let _span = tracing::info_span!("plan", %description).entered();
    let idx = classify(description);
    let tmpl = &TEMPLATES[idx].template;

    let scout_result = scout(store, embedder, description, root, limit)?;

    Ok(PlanResult {
        template: tmpl.name.to_string(),
        template_description: tmpl.description.to_string(),
        checklist: tmpl.checklist.iter().map(|s| s.to_string()).collect(),
        patterns: tmpl.patterns.iter().map(|s| s.to_string()).collect(),
        scout: scout_result,
    })
}

// ---------------------------------------------------------------------------
// JSON
// ---------------------------------------------------------------------------

/// Convert a PlanResult to JSON, including scout data.
///
/// Paths in the result are already relative to the project root.
pub fn plan_to_json(result: &PlanResult) -> serde_json::Value {
    let scout_json = crate::scout::scout_to_json(&result.scout);
    serde_json::json!({
        "template": result.template,
        "template_description": result.template_description,
        "checklist": result.checklist,
        "patterns": result.patterns,
        "scout": scout_json,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_language() {
        assert_eq!(classify("add dart language support"), 3);
        assert_eq!(TEMPLATES[3].template.name, "Add Language Support");
    }

    #[test]
    fn test_classify_bug() {
        assert_eq!(classify("fix broken search results"), 2);
        assert_eq!(TEMPLATES[2].template.name, "Fix a Bug");
    }

    #[test]
    fn test_classify_flag() {
        assert_eq!(classify("add --format flag to search"), 0);
        assert_eq!(TEMPLATES[0].template.name, "Add/Replace a CLI Flag");
    }

    #[test]
    fn test_classify_command() {
        assert_eq!(classify("add a new command for blame"), 1);
        assert_eq!(TEMPLATES[1].template.name, "Add a New CLI Command");
    }

    #[test]
    fn test_classify_injection() {
        assert_eq!(classify("add injection rule for Vue templates"), 5);
        assert_eq!(TEMPLATES[5].template.name, "Add Injection Rule");
    }

    #[test]
    fn test_classify_schema() {
        assert_eq!(classify("add schema migration for new column"), 9);
        assert_eq!(TEMPLATES[9].template.name, "Schema Migration");
    }

    #[test]
    fn test_classify_refactor() {
        assert_eq!(classify("refactor the store module"), 10);
        assert_eq!(TEMPLATES[10].template.name, "Refactor / Extract");
    }

    #[test]
    fn test_classify_performance() {
        assert_eq!(classify("optimize search performance"), 6);
        assert_eq!(TEMPLATES[6].template.name, "Performance Optimization");
    }

    #[test]
    fn test_classify_default() {
        // Ambiguous input falls back to Bug Fix
        assert_eq!(classify("improve the code quality"), 2);
    }

    #[test]
    fn test_template_names() {
        let names = template_names();
        assert_eq!(names.len(), 11);
        assert_eq!(names[0], "Add/Replace a CLI Flag");
    }
}
