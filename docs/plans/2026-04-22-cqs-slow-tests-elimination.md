# Eliminate the slow-tests nightly workflow

Status: spec
Date: 2026-04-22
Owner: jamie

## Problem

`.github/workflows/slow-tests.yml` runs nightly because the 5 CLI integration
test binaries (`cli_test`, `cli_batch_test`, `cli_commands_test`,
`cli_graph_test`, `cli_health_test` — 3,767 LOC, 110 tests total) take
~2 hours to run. The cost is dominated by `Command::cargo_bin("cqs")`
spawning the binary inside each test, which cold-loads the full
ONNX + HNSW + SPLADE stack every invocation.

Per-PR CI skips them via `#![cfg(feature = "slow-tests")]`, so regressions
land and only get caught the next morning. The slow-tests job has been
red multiple times in v1.28.x for unrelated drift; nobody notices for
hours because nobody watches the cron.

Goal: kill the cron, kill the feature gate, run all 110 tests in PR CI
in under a minute.

## Why subprocess-spawning was the wrong choice

Browse the test bodies and the same pattern dominates:

```rust
let dir = setup_project();
cqs().args(["init"]).current_dir(dir.path()).assert().success();
cqs().args(["index"]).current_dir(dir.path()).assert().success();
let output = cqs().args(["search", "add"]).current_dir(dir.path())
    .output().expect("...");
let json: Value = serde_json::from_slice(&output.stdout).unwrap();
assert!(json["results"][0]["name"] == "add");
```

Three subprocess spawns per test, three cold loads of the embedder. Every
one of those subcommands is a thin shim around a `pub fn` in the library
crate (`Store::search_filtered`, `Store::open`, `cqs::enumerate_files`,
etc.). The subprocess was convenience, not necessity.

The handful of things that *do* need a subprocess (argv parsing,
`--help`/`--version`, exit codes, stdout framing) are a small fraction of
the suite. Carve them off, run them in regular CI, delete the rest's
subprocess and call the library directly.

## Approach

Three phases, in order. Phases can land as separate PRs.

### Phase 1 — extract a shared in-process harness (~1 day)

Build `tests/common/mod.rs` (or `tests/in_process_fixture.rs`) with:

```rust
/// One-time-init harness for in-process integration tests. Spins up
/// a `Store`, `Embedder`, and `Parser` against a per-test tempdir,
/// indexes a fixture corpus, and exposes the trio for direct library
/// calls. Replaces the `cqs() + init + index + cqs() + assert()` pattern.
pub struct InProcessFixture {
    pub store: Store<ReadWrite>,
    pub embedder: Arc<Embedder>,
    pub parser: Parser,
    pub root: PathBuf,
    _tempdir: TempDir,  // dropped on Drop, deletes the tree
}

impl InProcessFixture {
    pub fn new() -> Self { ... }
    pub fn with_corpus(files: &[(&str, &str)]) -> Self { ... }
    pub fn reindex(&mut self) -> Result<()> { ... }
}
```

Key choices:
- **`Embedder` is shared via `Arc` across fixtures within a test binary.**
  Cold-load happens once per binary (5× total), not 110×. The model is
  read-only at inference time so sharing is safe.
- **Tempdir per fixture.** Maintains test isolation (each test owns its
  own `Store` + `.cqs/` dir). Drop reaps it.
- **Sample corpus reused.** `setup_project()`'s 2-function `lib.rs` is
  duplicated across 50+ tests; canonicalize it as `fixture::sample_rust()`.

Phase 1 deliverable: harness exists, lives in `tests/common/`, has
~3 unit tests of its own. No production code changes. No removal of
slow-tests yet.

### Phase 2 — convert tests one binary at a time (~3-5 days)

For each of the 5 slow-test binaries, in the order below:

1. **`cli_health_test.rs` (6 tests, smallest — pilot)**
   Convert `health` / `suggest` / `deps` to library calls. Health is
   already a library function (`cqs::health::*`). Largely mechanical.
   Land as PR-1.

2. **`cli_test.rs` (25 tests — biggest single)**
   The `init`, `index`, `search`, `stats` core. `init` becomes
   `Store::open(...)`; `index` becomes
   `enumerate_files + parser.parse_file + embedder.embed + store.upsert`;
   `search` becomes `store.search_filtered`; `stats` becomes
   `store.stats()`. Land as PR-2.

3. **`cli_graph_test.rs` (28 tests)** — `audit-mode`, `project`, `trace`,
   `gather`, `notes`. All library APIs exist. Land as PR-3.

4. **`cli_commands_test.rs` (20 tests)** — `scout`, `where`, `related`,
   `impact-diff`, `chat-completer`. All library APIs exist. Land as PR-4.

5. **`cli_batch_test.rs` (31 tests, hardest)**
   The batch REPL parses `pipe-syntax` (`search "foo" | callers | test-map`)
   and dispatches against the same library APIs as the rest of the CLI.
   Two paths:
   - **Easy path:** extract `BatchSession::execute_line(line) -> BatchResult`
     in production code; test it directly. The CLI surface stays a thin
     wrapper. Best long-term shape — also benefits the daemon, which
     speaks the same protocol.
   - **Quick path:** keep a few dozen "this command parses + dispatches"
     tests subprocess-spawning, gated behind a small `cli-surface-tests`
     feature, run in regular CI in <30 seconds (no model load needed
     because most batch tests don't actually retrieve).

   Pick easy path; the extraction is small and the result is a
   testable batch evaluator that the daemon can also invoke without
   reparsing.
   Land as PR-5.

After phase 2, every test that previously spawned a subprocess now calls
the library directly. Wall time per binary drops from ~25 minutes to
seconds.

### Phase 3 — replace slow-tests workflow + feature gate (~half a day)

After all 5 binaries are converted:

1. **New `tests/cli_surface_test.rs`** (~10 tests, no `slow-tests` gate):
   - `--help` output contains expected string
   - `--version` output starts with "cqs"
   - unknown subcommand exits 2
   - missing required arg exits 2
   - `--json` output is valid JSON
   - `cqs init` then `cqs init` is idempotent
   - `cqs serve --bind 0.0.0.0` warns about exposure
   - any other test that *genuinely* asserts on the binary surface

   These spawn `cqs --help`-style commands that don't load the model.
   Each takes ~50ms. Total binary <5 seconds.

2. **Delete `.github/workflows/slow-tests.yml`** entirely. Run all
   converted tests in `.github/workflows/ci.yml`'s `test` job (which
   already runs `cargo test --verbose`).

3. **Delete the `slow-tests` feature** from `Cargo.toml` line 254 + all
   `#![cfg(feature = "slow-tests")]` markers at the top of each test file.

4. **Issue #980** can be closed.

5. **Memory.md / CLAUDE.md** mentions of "slow-tests nightly" get
   removed.

## What to migrate to library calls (mapping)

| CLI command | Library API |
|---|---|
| `cqs init` | `Store::open(path)` then `store.init(&model_info)` |
| `cqs index` | `enumerate_files` → `parser.parse_file` → `embedder.embed_documents` → `store.upsert_chunks_batch` |
| `cqs search QUERY` | `store.search_filtered(&query_emb, &filter, n, threshold)` |
| `cqs callers FN` | `store.get_callers_full(fn_name)` |
| `cqs callees FN` | `store.get_callees_full(...)` |
| `cqs explain FN` | `cqs::explain(...)` |
| `cqs impact FN` | `cqs::impact_full(...)` (or `analyze_impact`) |
| `cqs scout QUERY` | `cqs::scout(...)` |
| `cqs gather QUERY` | `cqs::gather(...)` |
| `cqs where QUERY` | `cqs::where_to_add(...)` |
| `cqs related FN` | `cqs::related(...)` |
| `cqs trace FROM TO` | `cqs::trace(...)` |
| `cqs read PATH` | `cqs::focused_read(...)` |
| `cqs context FN` | `cqs::context(...)` |
| `cqs stats` | `store.stats()` |
| `cqs notes add/list/remove` | `cqs::note::*` |
| `cqs config get/set` | `cqs::config::*` |
| `cqs project register/list/remove` | `cqs::config::register_project(...)` |
| `cqs ref add/list/remove` | `cqs::reference::*` |
| `cqs gc` | `cqs::store::gc(...)` |
| `cqs stale` | `cqs::staleness::*` |
| `cqs audit-mode on/off` | `store.set_audit_mode(...)` |
| `cqs health` | `cqs::health::*` |
| `cqs doctor` | (likely subprocess — file/env probes; keep in cli-surface-tests) |
| `cqs batch` | `BatchSession::execute_line(...)` (NEW — extract from CLI) |

## Out of scope

- **Argv parsing tests.** clap's parser can be tested in-process (build
  a `Cli::parse_from(&["cqs", "..."])` and assert on the resulting
  struct), but those don't earn their keep — keep them in
  `cli_surface_test.rs` if they're high-value.
- **Pre-built shared corpus.** Tempting (use one `.cqs/` for all tests
  in a binary) but breaks isolation for tests that mutate the index.
  Keep per-test tempdirs.
- **Migrating other `tests/cli_*_test.rs` files** that aren't gated by
  `slow-tests`. Those are already fast. This work is targeted at the
  5 slow ones.
- **Daemon-mode integration tests.** Different concern; daemon is
  already tested via `daemon_translate` unit tests. Out of scope.

## Risks + mitigations

- **Risk: in-process tests miss bugs that only manifest as binary
  behavior** (argv parsing edge cases, panic-instead-of-exit, stdout
  buffering races). **Mitigation:** keep `cli_surface_test.rs` for the
  high-value subset. The other 100+ tests aren't testing those things
  anyway — they're testing search/graph/notes correctness, which lives
  in the library.

- **Risk: `setup_project()` and shared helpers leak into multiple test
  binaries during phase 2 transition.** **Mitigation:** put helpers in
  `tests/common/mod.rs` from the start so all binaries pick them up.

- **Risk: tests that genuinely need an isolated fresh process per
  invocation** (e.g., daemon socket lifecycle). **Mitigation:** identify
  during phase 2 review and keep them in `cli_surface_test.rs`. Estimate:
  fewer than 5 such tests across all 110.

- **Risk: phase 5 (batch) extraction is bigger than estimated.**
  **Mitigation:** if `BatchSession::execute_line` is a bigger refactor
  than expected, fall back to "quick path" — a small subset of batch
  tests stays subprocess-spawning, gated `cli-surface-tests` instead of
  `slow-tests`. Still solves the cron problem because the subprocess
  cost without model cold-load is small.

## Decision gates

- **After phase 1:** harness LOC + ergonomics. If `with_corpus(&[...])`
  feels like more code than the equivalent `setup_project + cqs init +
  cqs index`, redesign before going wide.
- **After PR-1 (cli_health_test):** measure wall time delta. If
  conversion of 6 tests cuts wall time of that binary from minutes to
  seconds, scale to the rest. If it doesn't (something model-load
  related is dominating), reassess.
- **After PR-5 (batch):** confirm regular `ci.yml`'s `test` job stays
  under 10 minutes. If converted slow-tests push it over, split into
  parallel jobs in CI before deleting `slow-tests.yml`.

## Estimated effort

| Phase | Effort | Output |
|---|---|---|
| 1. Harness | ~1 day | `tests/common/` module + tests of its own |
| 2.1 cli_health (PR-1) | ~half day | 6 tests converted, harness validated |
| 2.2 cli_test (PR-2) | ~1 day | 25 tests converted |
| 2.3 cli_graph (PR-3) | ~1 day | 28 tests converted |
| 2.4 cli_commands (PR-4) | ~1 day | 20 tests converted |
| 2.5 cli_batch (PR-5) | ~1.5 days (incl. BatchSession extraction) | 31 tests converted |
| 3. Cleanup | ~half day | slow-tests.yml deleted, feature gate removed, surface tests |

Total: ~6-7 days of focused work, parallelizable across agents per binary
in phase 2.

## Done state

- `cargo test --verbose` (no features needed) runs all 110 previously-slow
  tests in under a minute, alongside the rest of the suite
- `cli_surface_test.rs` has the ~10 tests that genuinely need a subprocess
- `slow-tests.yml`, `Cargo.toml`'s `slow-tests = []` feature, and the
  `#![cfg(feature = "slow-tests")]` markers are deleted
- Issue #980 is closed
