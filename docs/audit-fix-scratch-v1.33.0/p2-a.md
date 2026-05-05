# P2 Fix Prompts — Part A (P2.1 through P2.46)

Source: `docs/audit-triage.md` (P2 table) and `docs/audit-findings.md`. Each prompt
is self-contained; line numbers verified against the working tree on 2026-04-26.

> **Already-fixed callout:** P2.43 (`semantic_diff` tie-break), P2.44 (`is_structural_query` words check), P2.45 (`bfs_expand` seed sort), P2.46 (`contrastive_neighbors` tie-break) all already carry tie-break / words-loop fixes in the working tree. The prompts below for those four are **regression-pin tests only** — the implementation fix is already on disk.

---

## P2.1 — `cmd_similar` JSON parity drop

**Finding:** P2.1 in audit-triage.md
**Files:** `src/cli/batch/handlers/info.rs:139-148`
**Why:** Batch path emits 3 fields; CLI emits the canonical 9-field SearchResult shape via `r.to_json()`. Agents see a different schema for `cqs similar` depending on whether the daemon is up.

### Current code

```rust
let json_results: Vec<serde_json::Value> = filtered
    .iter()
    .map(|r| {
        serde_json::json!({
            "name": r.chunk.name,
            "file": normalize_path(&r.chunk.file),
            "score": r.score,
        })
    })
    .collect();
```

### Replacement

```rust
let json_results: Vec<serde_json::Value> =
    filtered.iter().map(|r| r.to_json()).collect();
```

### Notes

- `to_json()` is on `cqs::store::SearchResult` at `src/store/helpers/types.rs:143-156` and already normalizes the file path via the canonical helper.
- Add a snapshot test asserting CLI and batch produce identical key sets for the same query.

---

## P2.2 — `dispatch_diff` dead `target_store` placeholder

**Finding:** P2.2 in audit-triage.md
**Files:** `src/cli/batch/handlers/misc.rs:354-387`
**Why:** Dead-variable initialization plus duplicate `if target_label == "project"` match. The `get_ref` cached store at line 360 is loaded then discarded; the else branch reopens via `resolve_reference_store` at 378.

### Current code

```rust
let target_label = target.unwrap_or("project");
let target_store = if target_label == "project" {
    &ctx.store()
} else {
    ctx.get_ref(target_label)?;
    &ctx.store() // placeholder -- replaced below
};

// For non-project targets, resolve properly
let result = if target_label == "project" {
    cqs::semantic_diff(&source_store, target_store, source, target_label, threshold, lang)?
} else {
    let target_ref_store =
        crate::cli::commands::resolve::resolve_reference_store(&ctx.root, target_label)?;
    cqs::semantic_diff(&source_store, &target_ref_store, source, target_label, threshold, lang)?
};
```

### Replacement

```rust
let target_label = target.unwrap_or("project");
let result = if target_label == "project" {
    cqs::semantic_diff(
        &source_store,
        &ctx.store(),
        source,
        target_label,
        threshold,
        lang,
    )?
} else {
    let target_ref_store =
        crate::cli::commands::resolve::resolve_reference_store(&ctx.root, target_label)?;
    cqs::semantic_diff(
        &source_store,
        &target_ref_store,
        source,
        target_label,
        threshold,
        lang,
    )?
};
```

### Notes

- Drop the `ctx.get_ref(target_label)?` line entirely — it cached a store the code never reads. `resolve_reference_store` owns the lifetime here.
- One `if target_label == "project"` block, no placeholder, no duplicate match.

---

## P2.3 — Embedding/Query cache `open_with_runtime` 90+ line copy-paste

**Finding:** P2.3 in audit-triage.md
**Files:** `src/cache.rs:103-220` (`EmbeddingCache::open_with_runtime`), `src/cache.rs:1412-1522` (`QueryCache::open_with_runtime`)
**Why:** Two methods do parent-dir prep, runtime fallback, pool open, schema create, and 0o600 chmod loop in identical order with only `busy_timeout` differing. ~90 duplicated lines.

### Current code (sketch — both methods share this skeleton)

```rust
pub fn open_with_runtime(path: &Path, runtime: Option<Arc<Runtime>>) -> Result<Self, CacheError> {
    let _span = tracing::info_span!("EmbeddingCache::open", path = %path.display()).entered();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        #[cfg(unix)] {
            // 16 lines: chmod 0o700 best-effort with warn block
        }
    }
    let rt = if let Some(rt) = runtime { rt } else {
        // 9 lines: current_thread runtime fallback
    };
    let opts = SqliteConnectOptions::new().filename(path).create_if_missing(true)
        .busy_timeout(Duration::from_millis(5000))  // QueryCache: 2000
        .journal_mode(SqliteJournalMode::Wal);
    let pool = rt.block_on(SqlitePoolOptions::new().max_connections(1)
        .idle_timeout(Duration::from_secs(30)).connect_with(opts))?;
    rt.block_on(sqlx::query(SCHEMA_SQL).execute(&pool))?;
    #[cfg(unix)] {
        // 22 lines: 0o600 chmod loop on ["", "-wal", "-shm"]
    }
    // ... build Self
}
```

### Replacement

Extract three private helpers into a sibling module (e.g. `src/cache/open.rs` or inline at top of `cache.rs`):

```rust
#[cfg(unix)]
pub(super) fn prepare_cache_dir_perms(parent: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(parent) {
        let mut perms = meta.permissions();
        if perms.mode() & 0o777 != 0o700 {
            perms.set_mode(0o700);
            if let Err(e) = std::fs::set_permissions(parent, perms) {
                tracing::warn!(parent = %parent.display(), error = %e, "best-effort chmod 0o700 on cache dir failed");
            }
        }
    }
}

#[cfg(unix)]
pub(super) fn apply_db_file_perms(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    for suffix in ["", "-wal", "-shm"] {
        let p = path.with_extension(format!("{}{}", path.extension().and_then(|s| s.to_str()).unwrap_or(""), suffix));
        if let Ok(meta) = std::fs::metadata(&p) {
            let mut perms = meta.permissions();
            if perms.mode() & 0o777 != 0o600 {
                perms.set_mode(0o600);
                let _ = std::fs::set_permissions(&p, perms);
            }
        }
    }
}

pub(super) fn connect_cache_pool(
    path: &Path,
    busy_ms: u64,
    runtime: Option<Arc<Runtime>>,
    schema_sql: &str,
) -> Result<(SqlitePool, Arc<Runtime>), CacheError> {
    let rt = runtime.unwrap_or_else(|| {
        Arc::new(Builder::new_current_thread().enable_all().build()
            .expect("cache: failed to build current_thread runtime"))
    });
    let opts = SqliteConnectOptions::new().filename(path).create_if_missing(true)
        .busy_timeout(Duration::from_millis(busy_ms))
        .journal_mode(SqliteJournalMode::Wal);
    let pool = rt.block_on(async {
        let p = SqlitePoolOptions::new().max_connections(1)
            .idle_timeout(Duration::from_secs(30)).connect_with(opts).await?;
        sqlx::query(schema_sql).execute(&p).await?;
        Ok::<_, sqlx::Error>(p)
    })?;
    Ok((pool, rt))
}
```

Both `open_with_runtime` methods collapse to ~30 lines each, calling these three helpers.

### Notes

- Keep `Drop` impl panic-extraction unified via P2.4's helper too.
- Watch for the WAL/SHM filename quirk: SQLite tags them as `<basename>-wal`, not via `with_extension`. The existing code uses string concat (`path.to_string_lossy() + "-wal"`); preserve that semantic in `apply_db_file_perms`.

---

## P2.4 — `env::var(...).parse()` pattern at 25+ sites

**Finding:** P2.4 in audit-triage.md
**Files:** `src/limits.rs:230-260` (private helpers); duplicated at `src/cli/watch.rs:65,74,100,498,510,766,942,1430`, `src/llm/mod.rs:176,315,406,434`, `src/cli/pipeline/types.rs:80,98,117,144`, `src/hnsw/persist.rs:19,41,63`, `src/embedder/models.rs:565,571`, `src/embedder/mod.rs:330`, `src/cache.rs:206,1509`, `src/gather.rs:156`, `src/cli/commands/graph/trace.rs:357`, `src/impact/bfs.rs:16`, `src/reranker.rs:129`
**Why:** `parse_env_f32 / parse_env_usize / parse_env_u64` exist in `limits.rs` but are `pub(crate)`-private to `cqs::limits`. Every other module re-rolls the pattern with slightly different zero-handling and warn behavior. P2.5 (cache zero-divergence) is a direct consequence.

### Replacement plan

1. Promote the three helpers in `src/limits.rs` to `pub`, and add a `parse_env_duration_secs` variant. Keep the existing `pub(crate)` re-exports so internal callers don't break.
2. Replace the 25+ open-coded sites with calls. Example for `src/cli/pipeline/types.rs:143`:

```rust
// Before
match std::env::var("CQS_EMBED_BATCH_SIZE") {
    Ok(val) => match val.parse::<usize>() { Ok(s) if s > 0 => s, _ => 64 },
    Err(_) => 64,
}
// After
cqs::limits::parse_env_usize("CQS_EMBED_BATCH_SIZE", 64).max(1)
```

### Notes

- Decide zero-handling **once** in the helper signature (e.g. `parse_env_usize_nonzero`); P2.5 is a known divergence to fold into the same PR.
- Don't change observable behavior in this PR — match the existing default at every call site even where the new helper would be cleaner. Behavior changes ride on a follow-up.

---

## P2.5 — `EmbeddingCache` accepts `CQS_CACHE_MAX_SIZE=0`; `QueryCache` rejects

**Finding:** P2.5 in audit-triage.md
**Files:** `src/cache.rs:206-209` (Embedding, no zero filter), `src/cache.rs:1509-1513` (Query, `.filter(|&n: &u64| n > 0)`)

### Current code

```rust
// EmbeddingCache (line 206-209)
let max_size_bytes = std::env::var("CQS_CACHE_MAX_SIZE")
    .ok()
    .and_then(|s| s.parse().ok())
    .unwrap_or(10 * 1024 * 1024 * 1024); // 10GB default

// QueryCache (line 1509-1513)
let max_size_bytes = std::env::var("CQS_QUERY_CACHE_MAX_SIZE")
    .ok()
    .and_then(|s| s.parse().ok())
    .filter(|&n: &u64| n > 0)
    .unwrap_or(100 * 1024 * 1024);
```

### Replacement

Pick semantic: "0 is invalid → fall back to default". Add the filter to `EmbeddingCache`:

```rust
let max_size_bytes = std::env::var("CQS_CACHE_MAX_SIZE")
    .ok()
    .and_then(|s| s.parse().ok())
    .filter(|&n: &u64| n > 0)
    .unwrap_or(10 * 1024 * 1024 * 1024);
```

### Notes

- Folds naturally into the P2.4 helper sweep — drive both through `parse_env_u64_nonzero` once that helper lands.
- Document chosen semantic at the env-var site.

---

## P2.6 / P2.7 / P2.9 / P2.10 — README documentation drift (combined)

**Finding:** P2.6, P2.7, P2.9, P2.10 in audit-triage.md
**Files:** `README.md:5,467-525,521,530-585,649-653`
**Why:** TL;DR claims "544-query eval" (actual 218); claims 54 languages but Elm missing from list; Claude Code Integration block omits 5 commands and lists `stats/prune/compact` instead of `stats/clear/prune/compact`.

### Current code (README.md:5)

```markdown
**TL;DR:** Code intelligence toolkit for Claude Code. ... 17-41x token reduction vs full file reads. **42.2% R@1 / 67.0% R@5 / 83.5% R@20 on a 544-query dual-judge eval against the cqs codebase itself** (BGE-large dense + SPLADE sparse with per-category fusion + centroid query routing). 54 languages + L5X/L5K PLC exports, GPU-accelerated.
```

### Replacement (TL;DR — P2.6, P2.7)

```markdown
**TL;DR:** Code intelligence toolkit for Claude Code. ... 17-41x token reduction vs full file reads. **42.2% R@1 / 67.0% R@5 / 83.5% R@20 on a 218-query dual-judge eval (109 test + 109 dev, v3.v2 fixture) against the cqs codebase itself** (BGE-large dense + SPLADE sparse with per-category fusion + centroid query routing). 54 languages + L5X/L5K PLC exports, GPU-accelerated.
```

Plus add an `Elm` bullet to the alphabetical list under `## Supported Languages (54)` between `Dockerfile` and `Erlang` (or wherever alphabetically).

### Replacement (Claude Code Integration — P2.9, P2.10)

In `README.md:521`:

```markdown
- `cqs cache stats/clear/prune/compact` - manage the project-scoped embeddings cache at `<project>/.cqs/embeddings_cache.db`. `--per-model` on stats; `clear --model <fp>` deletes all cached embeddings for one fingerprint; `prune <DAYS>` or `prune --model <id>`; `compact` runs VACUUM
```

Add five new bullets in the alphabetically-correct positions inside the Code Intelligence block (lines 467-525):

```markdown
- `cqs ping` - daemon healthcheck; reports daemon socket path and uptime if running
- `cqs eval <fixture>` - run a query fixture against the current index and emit R@K metrics. `--baseline <path>` to compare two reports
- `cqs model show/list/swap` - inspect the embedding model recorded in the index, list presets, or swap with restore-on-failure semantics
- `cqs serve [--bind ADDR]` - launch the read-only web UI (graph, hierarchy, cluster, chunk-detail). Per-launch auth token; banner prints the URL
- `cqs refresh` - invalidate daemon caches and re-open the Store. Alias `cqs invalidate`. No-op when no daemon is running
```

### Notes

- Eval metrics: optionally also bump to refreshed v3.v2 numbers (`63.3% R@5 test, 74.3% R@5 dev`) per memory file `feedback_eval_line_start_drift.md`. Keep both numbers consistent if updating.
- Verify language count by walking `define_languages!` macro in `src/language/mod.rs` — fix the README header to match instead of guessing.

---

## P2.8 — SECURITY.md omits per-project embeddings_cache.db

**Finding:** P2.8 in audit-triage.md
**Files:** `SECURITY.md:65-82` (Read Access table)

### Current code

```markdown
| `~/.cache/cqs/embeddings.db` | Global embedding cache (content-addressed, capped at 1 GB) | Index and search |
| `~/.cache/cqs/query_cache.db` | Recent query embedding cache (7-day TTL) | Search |
```

### Replacement

Add row to both Read Access (line 65-82) and Write Access (line 83-95) tables:

```markdown
| `<project>/.cqs/embeddings_cache.db` | Per-project embedding cache (PR #1105, primary; legacy global cache at `~/.cache/cqs/embeddings.db` is fallback) | `cqs index`, search |
```

Also fix the misleading "7-day TTL" claim — see P1.2 (PRIVACY.md fix); keep SECURITY consistent: `Recent query embedding cache (size-capped at CQS_QUERY_CACHE_MAX_SIZE, 100 MiB default)`.

### Notes

- Cross-check against PRIVACY.md so wording matches; the same per-project path is documented correctly there at lines 16-20.

---

## P2.11 — `cqs --json model swap/show` emits plain-text errors

**Finding:** P2.11 in audit-triage.md
**Files:** `src/cli/commands/infra/model.rs:144-149` (`cmd_model_show` bail), `src/cli/commands/infra/model.rs:256-261` (`cmd_model_swap` Unknown preset), `src/cli/commands/infra/model.rs:267-272` (`cmd_model_swap` no-index bail)

### Current code

```rust
// cmd_model_show, line 144-149
if !index_path.exists() {
    bail!(
        "No index at {}. Run `cqs init && cqs index` first.",
        index_path.display()
    );
}

// cmd_model_swap, line 256-272
let new_cfg = ModelConfig::from_preset(preset).ok_or_else(|| {
    let valid = ModelConfig::PRESET_NAMES.join(", ");
    anyhow::anyhow!(
        "Unknown preset '{preset}'. Valid presets: {valid}. Run `cqs model list` for repos."
    )
})?;

if !index_path.exists() {
    bail!(
        "No index at {}. Run `cqs init && cqs index --model {preset}` first.",
        index_path.display()
    );
}
```

### Replacement

Route through `crate::cli::json_envelope::emit_json_error` when `json` is true:

```rust
// cmd_model_show
if !index_path.exists() {
    let msg = format!("No index at {}. Run `cqs init && cqs index` first.", index_path.display());
    if json {
        crate::cli::json_envelope::emit_json_error("no_index", &msg)?;
        std::process::exit(1);
    }
    bail!("{msg}");
}

// cmd_model_swap — Unknown preset
let new_cfg = match ModelConfig::from_preset(preset) {
    Some(c) => c,
    None => {
        let valid = ModelConfig::PRESET_NAMES.join(", ");
        let msg = format!("Unknown preset '{preset}'. Valid presets: {valid}. Run `cqs model list` for repos.");
        if json {
            crate::cli::json_envelope::emit_json_error("unknown_preset", &msg)?;
            std::process::exit(1);
        }
        bail!("{msg}");
    }
};

// cmd_model_swap — no index (mirror cmd_model_show fix)
```

Also add `already_on_target` code for the no-op short-circuit and `swap_failed` for the post-rebuild restore path.

### Notes

- Pattern matches `cmd_ref_remove` in `src/cli/commands/infra/reference.rs` which the v1.30.0 audit already standardized.
- Tests: snapshot a `cqs --json model swap nonexistent` against the canonical envelope shape `{data: null, error: {code, message}, version: 1}`.

---

## P2.12 — `cqs init / index / convert` lack `--json`

**Finding:** P2.12 in audit-triage.md
**Files:** `src/cli/commands/infra/init.rs:13` (`cmd_init`), `src/cli/commands/index/build.rs::cmd_index`, `src/cli/commands/index/convert.rs::cmd_convert`, `src/cli/args.rs::IndexArgs`, `src/cli/args.rs` (no `InitArgs` / `ConvertArgs` struct yet)

### Replacement

1. Add `pub json: bool` to `IndexArgs`, introduce `InitArgs { #[arg(long)] pub json: bool }`, add `pub json: bool` to `ConvertArgs`.
2. Thread `cli.json || args.json` into `cmd_init` / `cmd_index` / `cmd_convert`.
3. After work completes, emit a final summary envelope:

```rust
// cmd_init
if json {
    let obj = serde_json::json!({
        "initialized": true,
        "cqs_dir": cqs_dir.display().to_string(),
        "model": effective_model_name,
    });
    crate::cli::json_envelope::emit_json(&obj)?;
}

// cmd_index
if json {
    let obj = serde_json::json!({
        "indexed_files": file_count,
        "indexed_chunks": chunk_count,
        "took_ms": elapsed.as_millis(),
        "model": model_name,
        "summaries_added": summaries_added,
        "docs_improved": docs_improved,
    });
    crate::cli::json_envelope::emit_json(&obj)?;
}

// cmd_convert
if json {
    let obj = serde_json::json!({
        "converted": converted_paths,
        "skipped": skipped_paths,
        "took_ms": elapsed.as_millis(),
    });
    crate::cli::json_envelope::emit_json(&obj)?;
}
```

4. Suppress per-step progress prints when `json` is true (route to stderr or gate on `!json`).

### Notes

- `cmd_doctor` already has a `Colored human-readable check progress is routed to stderr in this mode` pattern — reuse it.
- These three commands already emit useful progress; do not regress that for the text path.

---

## P2.13 — Global `--slot` silently ignored by `slot` and `cache` subcommands

**Finding:** P2.13 in audit-triage.md
**Files:** `src/cli/definitions.rs` (`pub slot: Option<String>` is `global = true`); consumers: `src/cli/commands/infra/slot.rs` (`SlotCommand::*`), `src/cli/commands/infra/cache_cmd.rs::resolve_cache_path`

### Replacement

Two acceptable shapes — pick at PR time:

**Option A (recommended):** Move `--slot` off `global = true` and onto only the subcommands that consume it. Apply via `#[command(flatten)]` of a `SlotArg { #[arg(long)] pub slot: Option<String> }` to: `Search`, `Index`, `Doctor`, `ModelSwap`, etc. Drop from `Slot*`/`Cache*`.

**Option B:** Keep global but enforce at dispatch:

```rust
// In dispatch routing for Slot* / Cache* arms:
if cli.slot.is_some() {
    bail!("--slot has no effect on `cqs {subcommand}` (this command is project-scoped, not slot-scoped)");
}
```

### Notes

- Cache is project-scoped per #1105, not per-slot — that contract is the source of the bug. Don't accidentally make it slot-scoped without an explicit design decision.
- Bonus cleanup: `cqs slot create foo --slot bar` is parsed as "create slot foo" today; option A makes the misuse a clap error.

---

## P2.14 — `cqs refresh` has no `--json`

**Finding:** P2.14 in audit-triage.md
**Files:** `src/cli/definitions.rs:759-760` (`Commands::Refresh` — variant has no fields); `src/cli/registry.rs` Refresh dispatch arm; daemon-side `dispatch_refresh` handler

### Current code

```rust
#[command(visible_alias = "invalidate")]
Refresh,
```

### Replacement

```rust
#[command(visible_alias = "invalidate")]
Refresh {
    #[arg(long)]
    json: bool,
},
```

In the dispatch arm, when `json` is set emit:

```rust
let obj = serde_json::json!({
    "refreshed": true,
    "daemon_running": daemon_was_running,
    "caches_invalidated": ["embedder_session", "query_cache_lru", "notes"],
});
crate::cli::json_envelope::emit_json(&obj)?;
```

### Notes

- `cli.json || args.json` precedence so the global flag still works.
- Update the registry `for_each_command!` row if pattern-matching on field shape.

---

## P2.15 — List-shape JSON envelopes inconsistent across `*list` commands

**Finding:** P2.15 in audit-triage.md
**Files:** `src/cli/commands/infra/reference.rs::cmd_ref_list` (raw array), `src/cli/commands/infra/model.rs::cmd_model_list` (raw array), `src/cli/commands/infra/project.rs:124-140` (object), `src/cli/commands/infra/slot.rs::slot_list` (object), `src/cli/commands/io/notes.rs::cmd_notes_list` (object), `src/cli/commands/search/query.rs` (object)

### Replacement

Standardize on `{"data": {"<plural>": [...], <optional summary fields>}}`:

```rust
// cmd_ref_list — change from raw Vec<RefSummary> to:
let obj = serde_json::json!({ "references": refs });
crate::cli::json_envelope::emit_json(&obj)?;

// cmd_model_list — change from raw Vec<ModelInfo> to:
let obj = serde_json::json!({
    "models": models,
    "current": current_name, // fold the per-row `current: bool` into a top-level field
});
crate::cli::json_envelope::emit_json(&obj)?;
```

### Notes

- One PR touches both call sites + their tests. No external users — hard rename is fine per project memory.
- After the rename, `data.{slots,projects,refs,models,notes}` is a uniform accessor.

---

## P2.16 — `cache stats` mixes bytes and MB; `cache compact` uses bytes only

**Finding:** P2.16 in audit-triage.md
**Files:** `src/cli/commands/infra/cache_cmd.rs::cache_stats` (around line 145-149, emits both `total_size_bytes` AND `total_size_mb`), `cache_compact` (bytes only)

### Current code (cache_stats JSON branch)

```rust
let obj = serde_json::json!({
    "cache_path": cache_path.display().to_string(),
    "total_entries": stats.total_entries,
    "total_size_bytes": stats.total_size_bytes,
    "total_size_mb": stats.total_size_bytes as f64 / 1_048_576.0,
    // ...
});
```

### Replacement

```rust
let obj = serde_json::json!({
    "cache_path": cache_path.display().to_string(),
    "total_entries": stats.total_entries,
    "total_size_bytes": stats.total_size_bytes,
    // total_size_mb dropped — bytes is the canonical unit
    // ...
});
```

Keep the `total_size_mb` rendering only on the human text path (`format!("{:.1} MB", ...)`).

### Notes

- Repository-wide grep for consumers of `total_size_mb` before dropping. None expected (no external users).

---

## P2.17 — `dispatch::try_daemon_query` warns then silently re-runs in CLI

**Finding:** P2.17 in audit-triage.md
**Files:** `src/cli/dispatch.rs:445-462`
**Why:** EH-13 comment claims "no silent fallback" but the function returns `None` to fall through to CLI anyway. Daemon-only features can produce different results between the warning print and the CLI re-run.

### Current code

```rust
// EH-13: daemon understood the request but surfaced an error. ... Falling
// back to CLI now would mask daemon bugs ...
let msg = resp.get("message").and_then(|v| v.as_str()).unwrap_or("daemon error");
tracing::warn!(error = msg, "Daemon returned protocol-level error");
eprintln!("cqs: daemon error: {msg}");
eprintln!("hint: set CQS_NO_DAEMON=1 to run the command directly in the CLI (bypasses the daemon).");
// Still return None so we fall through to CLI path, but the user has been
// told why — no silent fallback.
None
```

### Replacement

Change the return type so the protocol-error case bubbles up instead of falling through:

```rust
// File header: change signature
fn try_daemon_query(...) -> Result<Option<String>, anyhow::Error> { ... }

// At the EH-13 branch:
let msg = resp.get("message").and_then(|v| v.as_str()).unwrap_or("daemon error");
tracing::warn!(error = msg, "Daemon returned protocol-level error");
return Err(anyhow::anyhow!(
    "daemon error: {msg}\nhint: set CQS_NO_DAEMON=1 to bypass the daemon"
));
```

Caller at `:200` switches from `match try_daemon_query(...) { Some(s) => ..., None => fall_through_to_cli() }` to handling the new `Result<Option<String>>`:

```rust
match try_daemon_query(...)? {
    Some(text) => emit(text),
    None => fall_through_to_cli(),
}
```

### Notes

- Exits non-zero with the daemon's message — matches the comment's stated intent.
- All existing transport-error paths (connect/read/write fail) still return `Ok(None)` so CLI fallback works for those.

---

## P2.18 — `LocalProvider::fetch_batch_results` returns empty map on missing batch_id

**Finding:** P2.18 in audit-triage.md
**Files:** `src/llm/local.rs:542-547`

### Current code

```rust
fn fetch_batch_results(&self, batch_id: &str) -> Result<HashMap<String, String>, LlmError> {
    // Drain the stash entry — returning empty if the id was already
    // fetched or never existed.
    let mut stash = self.stash.lock().unwrap();
    Ok(stash.remove(batch_id).unwrap_or_default())
}
```

### Replacement

```rust
fn fetch_batch_results(&self, batch_id: &str) -> Result<HashMap<String, String>, LlmError> {
    let mut stash = self.stash.lock().unwrap_or_else(|p| p.into_inner());
    match stash.remove(batch_id) {
        Some(m) => Ok(m),
        None => Err(LlmError::BatchNotFound(format!(
            "local batch_id {batch_id} not found in stash — already fetched, or submission silently lost results"
        ))),
    }
}
```

Add the variant at the top of `src/llm/error.rs` (or wherever `LlmError` is defined):

```rust
#[error("batch not found: {0}")]
BatchNotFound(String),
```

### Notes

- Mutex poison fix piggybacks here (recover via `into_inner` instead of `unwrap`) — same lesson as P1.9.
- Callers in `summary.rs` / `doc_comments.rs` should distinguish `BatchNotFound` (data drift, hard error) from `Http`/`Internal` (transient).

---

## P2.19 — `serde_json::to_value(...).unwrap_or_else(json!({}))` at 6 sites

**Finding:** P2.19 in audit-triage.md
**Files:** `src/impact/format.rs:11-16, 101-106`, `src/cli/commands/io/context.rs:94-97, 320-323, 498-501`, `src/cli/commands/io/blame.rs:240-243`

### Current code (representative — `src/impact/format.rs:11-16`)

```rust
pub fn impact_to_json(result: &ImpactResult) -> serde_json::Value {
    serde_json::to_value(result).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "Failed to serialize ImpactResult");
        serde_json::json!({})
    })
}
```

### Replacement

```rust
pub fn impact_to_json(result: &ImpactResult) -> Result<serde_json::Value, serde_json::Error> {
    serde_json::to_value(result)
}
```

Apply same shape to all six sites. Bump call sites to `?`-propagate. The functions all already terminate in `crate::cli::json_envelope::emit_json(&obj)?` which handles `Result`.

### Notes

- `serde_json::to_value` only fails on `Serialize` impl bugs — these are programmer errors, must fail loud, not produce `{}` and a journal warn.
- Six near-identical changes; ship as one PR.

---

## P2.20 — `cache_stats` silently treats `QueryCache::open` failure as 0 bytes

**Finding:** P2.20 in audit-triage.md
**Files:** `src/cli/commands/infra/cache_cmd.rs:120-139`

### Current code

```rust
let query_cache_size_bytes: u64 = {
    let q_path = QueryCache::default_path();
    if q_path.exists() {
        match QueryCache::open(&q_path) {
            Ok(qc) => qc.size_bytes().unwrap_or_else(|e| {
                tracing::warn!(error = %e, "Query cache size_bytes failed");
                0
            }),
            Err(e) => {
                tracing::warn!(error = %e, "Query cache open failed for stats");
                0
            }
        }
    } else {
        0
    }
};
```

### Replacement

Surface the error as a structured field instead of collapsing to 0:

```rust
let (query_cache_size_bytes, query_cache_status): (u64, String) = {
    let q_path = QueryCache::default_path();
    if !q_path.exists() {
        (0, "missing".to_string())
    } else {
        match QueryCache::open(&q_path) {
            Ok(qc) => match qc.size_bytes() {
                Ok(n) => (n, "ok".to_string()),
                Err(e) => {
                    tracing::warn!(error = %e, "Query cache size_bytes failed");
                    (0, format!("error: {e}"))
                }
            },
            Err(e) => {
                tracing::warn!(error = %e, "Query cache open failed for stats");
                (0, format!("error: {e}"))
            }
        }
    }
};
```

Add `query_cache_status` to the JSON envelope and to the text output.

---

## P2.21 — `slot_remove` masks `list_slots` failure as "only slot remaining"

**Finding:** P2.21 in audit-triage.md
**Files:** `src/cli/commands/infra/slot.rs:303-313`

### Current code

```rust
let active = read_active_slot(project_cqs_dir).unwrap_or_else(|| DEFAULT_SLOT.to_string());
let mut all = list_slots(project_cqs_dir).unwrap_or_default();
all.retain(|n| n != name);

if name == active {
    if all.is_empty() {
        anyhow::bail!(
            "Refusing to remove the only remaining slot '{}'. Create another slot first.",
            name
        );
    }
    // ...
}
```

### Replacement

```rust
use anyhow::Context;

let active = read_active_slot(project_cqs_dir).unwrap_or_else(|| DEFAULT_SLOT.to_string());
let mut all = list_slots(project_cqs_dir)
    .context("Failed to list slots while validating remove")?;
all.retain(|n| n != name);
```

### Notes

- Same pattern at `src/cli/commands/infra/slot.rs:273` (`slot_promote`), `:304` (already), and `src/cli/commands/infra/doctor.rs:923` — sweep them all in this PR.

---

## P2.22 — `build_token_pack` swallows `get_caller_counts_batch` error

**Finding:** P2.22 in audit-triage.md
**Files:** `src/cli/commands/io/context.rs:438-441`

### Current code

```rust
let caller_counts = store.get_caller_counts_batch(&names).unwrap_or_else(|e| {
    tracing::warn!(error = %e, "Failed to fetch caller counts for token packing");
    HashMap::new()
});
let (included, used) = pack_by_relevance(chunks, &caller_counts, budget, &embedder);
```

### Replacement

`build_token_pack` already returns `Result`; propagate:

```rust
let caller_counts = store.get_caller_counts_batch(&names)
    .context("Failed to fetch caller counts for token packing — ranking signal required")?;
let (included, used) = pack_by_relevance(chunks, &caller_counts, budget, &embedder);
```

### Notes

- Packing without ranking signal is worse than failing the command — current behavior silently degrades to file-order with no signal in JSON output.
- Sibling `build_full_data` carries a `warnings` field; consider folding this into that pattern instead of `?` if the caller wants degraded output. The propagation path is preferred per the finding.

---

## P2.23 — `read --focus` silently empties `type_chunks` on store batch failure

**Finding:** P2.23 in audit-triage.md
**Files:** `src/cli/commands/io/read.rs:230-235`

### Current code

```rust
let batch_results = store
    .search_by_names_batch(&type_names, 5)
    .unwrap_or_else(|e| {
        tracing::warn!(error = %e, "Failed to batch-lookup type definitions for focused read");
        std::collections::HashMap::new()
    });
```

### Replacement

Add a `warnings: Vec<String>` field to `FocusedReadOutput` and push when fetch fails:

```rust
let batch_results = match store.search_by_names_batch(&type_names, 5) {
    Ok(m) => m,
    Err(e) => {
        let msg = format!("search_by_names_batch failed: {e}; type definitions omitted");
        tracing::warn!(error = %e, "Failed to batch-lookup type definitions for focused read");
        warnings.push(msg);
        std::collections::HashMap::new()
    }
};
```

In the typed output struct:

```rust
#[derive(Serialize)]
struct FocusedReadOutput {
    // ... existing fields
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
}
```

### Notes

- Mirrors `SummaryOutput::warnings` in `src/cli/commands/io/context.rs:464`.
- Either propagation (`?`) or warnings-field is acceptable; warnings-field preferred per the EH-V1.29-9 family.

---

## P2.24 — `serve::build_chunk_detail` collapses NULL signature/content to empty string

**Finding:** P2.24 in audit-triage.md
**Files:** `src/serve/data.rs:488-492`

### Current code

```rust
let signature: String = row
    .get::<Option<String>, _>("signature")
    .unwrap_or_default();
let doc: Option<String> = row.get("doc");
let content: String = row.get::<Option<String>, _>("content").unwrap_or_default();
```

### Replacement

```rust
let signature: Option<String> = row.get("signature");
let doc: Option<String> = row.get("doc");
let content: Option<String> = row.get("content");
```

Update `ChunkDetail` struct to `signature: Option<String>` and `content: Option<String>`. Update `src/serve/assets/views/chunk-detail.js` to render `null` as a `<missing — DB column NULL>` placeholder instead of an empty pane.

### Notes

- NULL is a real signal (partial write during indexing, SIGKILL between INSERT phases) — flattening to `""` loses it.
- The `content_preview` derivation at line 496 changes too: `content.as_deref().map(|c| c.lines().take(30)...)`.

---

## P2.25 — Per-request span and `build_*` spans disconnected via `spawn_blocking`

**Finding:** P2.25 in audit-triage.md
**Files:** `src/serve/handlers.rs:86,111,131,160,210,236` (every `spawn_blocking` call)

### Current code (representative — handlers.rs:86)

```rust
let store = state.store.clone();
let stats = tokio::task::spawn_blocking(move || super::data::build_stats(&store))
    .await
    .map_err(|e| ServeError::Internal(format!("stats join: {e}")))?
    .map_err(ServeError::from)?;
```

### Replacement

Capture the calling span and re-enter inside the closure:

```rust
use tracing::Instrument;
let span = tracing::Span::current();
let store = state.store.clone();
let stats = tokio::task::spawn_blocking({
    move || {
        let _entered = span.enter();
        super::data::build_stats(&store)
    }
})
.await
.map_err(|e| ServeError::Internal(format!("stats join: {e}")))?
.map_err(ServeError::from)?;
```

Apply at all six handlers (stats / graph / chunk_detail / search / hierarchy / cluster_2d).

### Notes

- After this lands, drop the per-handler `tracing::info!` lines (`80, 100, 126, 149, 175, 201, 231`) — the inner `build_*` span entry plus `FmtSpan::CLOSE` will produce one structured event per request.
- Ties to P1.20 (default subscriber drops INFO spans) — the value of this fix only materializes after that one ships.

---

## P2.26 — TC-ADV: `LocalProvider` body-size DoS test

**Finding:** P2.26 in audit-triage.md
**Files:** `src/llm/local.rs:474-500` (production); `src/llm/local.rs:595+` (existing tests module)

### Test skeleton

```rust
#[cfg(test)]
mod body_size_dos_tests {
    use super::*;
    use httpmock::prelude::*;

    #[test]
    fn test_oversized_response_body_capped_at_5mb() {
        let server = MockServer::start();
        // Mock 200-OK with a 50 MB JSON body
        let huge_body: String = "{\"choices\":[{\"message\":{\"content\":\"".to_string()
            + &"x".repeat(50 * 1024 * 1024)
            + "\"}}]}";
        let _m = server.mock(|when, then| {
            when.method(POST).path("/v1/chat/completions");
            then.status(200).body(huge_body);
        });
        let cfg = make_config(&format!("{}/v1", server.base_url()), Duration::from_secs(5));
        let provider = LocalProvider::new(&cfg).unwrap();
        let items = vec![/* one minimal chat item */];
        let start = std::time::Instant::now();
        let result = provider.submit_batch_prebuilt(&items, None);
        // Expectation: either errors out with a body-size cap, or completes in
        // bounded memory (we cannot assert allocator behavior portably; the
        // PR-side fix should add a 4 MiB cap via reqwest body limits).
        assert!(
            result.is_err() || start.elapsed() < Duration::from_secs(30),
            "unbounded body read or excessive retry stall"
        );
    }

    #[test]
    fn test_4xx_with_large_body_does_not_buffer_entire_body() {
        let server = MockServer::start();
        let _m = server.mock(|when, then| {
            when.method(POST).path("/v1/chat/completions");
            then.status(400).body("x".repeat(50 * 1024 * 1024));
        });
        let cfg = make_config(&format!("{}/v1", server.base_url()), Duration::from_secs(5));
        let provider = LocalProvider::new(&cfg).unwrap();
        // body_preview should produce ≤ 256 chars; assert it doesn't OOM
        // and returns a bounded preview.
        let result = provider.submit_batch_prebuilt(&[/* one item */], None);
        assert!(result.is_err());
        // Optional: peek into the LlmError variant and assert preview length
    }
}
```

### Notes

- Production-side fix (per RB-V1.30-1): add `Content-Length` inspection or a `take(N)` adaptor with a 4 MiB cap on summary responses, 2 KiB on `body_preview`. New env var `CQS_LOCAL_LLM_MAX_BODY_BYTES`.
- Tests remain meaningful even if the fix is deferred — they pin current unbounded behavior so a regression after the fix is loud.

---

## P2.27 — TC-ADV: cache accepts NaN/Inf embeddings

**Finding:** P2.27 in audit-triage.md
**Files:** `src/cache.rs:332-407` (`EmbeddingCache::write_batch`), `src/cache.rs:1677-1699` (`QueryCache::put`)

### Test skeleton

```rust
#[cfg(test)]
mod nan_inf_tests {
    use super::*;

    #[test]
    fn test_write_batch_rejects_nan_embedding() {
        let dir = tempfile::tempdir().unwrap();
        let cache = EmbeddingCache::open(&dir.path().join("emb.db")).unwrap();
        let bad = vec![1.0_f32, f32::NAN, 0.5, /* pad to dim */];
        // Pad bad to expected dim
        let dim = bad.len();
        let entries = &[("a".repeat(64).as_str(), bad.as_slice())];
        let written = cache.write_batch(entries, "fp1", dim).unwrap();
        assert_eq!(written, 0, "NaN embedding must be rejected, not silently stored");
        // Read back must return no row for that hash
        let got = cache.read_batch(&[&"a".repeat(64)], "fp1", dim).unwrap();
        assert!(got.is_empty(), "rejected entry must not appear in read_batch");
    }

    #[test]
    fn test_write_batch_rejects_inf_embedding() {
        let dir = tempfile::tempdir().unwrap();
        let cache = EmbeddingCache::open(&dir.path().join("emb.db")).unwrap();
        let bad = vec![f32::INFINITY; 16];
        let entries = &[("b".repeat(64).as_str(), bad.as_slice())];
        let written = cache.write_batch(entries, "fp1", 16).unwrap();
        assert_eq!(written, 0);
        let bad2 = vec![f32::NEG_INFINITY; 16];
        let entries2 = &[("c".repeat(64).as_str(), bad2.as_slice())];
        let written2 = cache.write_batch(entries2, "fp1", 16).unwrap();
        assert_eq!(written2, 0);
    }

    #[test]
    fn test_query_cache_put_rejects_non_finite() {
        // Same shape against QueryCache::put
    }
}
```

### Notes

- Production fix: add `if embedding.iter().any(|f| !f.is_finite()) { tracing::warn!(...); continue; }` next to the existing `embedding.len() != dim` skip block in both write paths.
- #1105 made this worse by extending cache lifetime cross-slot.

---

## P2.28 — TC-ADV: slot create/remove TOCTOU under concurrent operation

**Finding:** P2.28 in audit-triage.md
**Files:** `src/cli/commands/infra/slot.rs:219-266` (slot_create), `src/cli/commands/infra/slot.rs:299-350` (slot_remove); existing tests at `:391-516`

### Test skeleton

```rust
#[cfg(test)]
mod toctou_tests {
    use super::*;

    #[test]
    fn test_slot_create_concurrent_same_name() {
        let dir = tempfile::tempdir().unwrap();
        let cqs_dir = dir.path().join(".cqs");
        std::fs::create_dir_all(&cqs_dir).unwrap();
        let dir1 = cqs_dir.clone();
        let dir2 = cqs_dir.clone();
        let h1 = std::thread::spawn(move || slot_create(&dir1, "foo", Some("bge-large"), false));
        let h2 = std::thread::spawn(move || slot_create(&dir2, "foo", Some("e5-base"), false));
        let r1 = h1.join().unwrap();
        let r2 = h2.join().unwrap();
        // Contract: at most one returns Ok; the slot ends up with a deterministic model.
        assert!(r1.is_ok() ^ r2.is_ok() || (r1.is_ok() && r2.is_err()) || (r1.is_err() && r2.is_ok()));
        let model = read_slot_model(&cqs_dir, "foo");
        assert!(model == Some("bge-large".into()) || model == Some("e5-base".into()));
    }

    #[test]
    fn test_slot_remove_during_open_index_db() {
        // Open a Store::open_readonly_pooled on slots/foo/index.db, spawn slot_remove
        // from another thread, assert either the open store keeps working OR the remove
        // returns an error. Currently neither is guaranteed.
    }
}
```

### Notes

- Production fix: `flock` on `.cqs/slots.lock` acquired by both `slot_remove` and the indexer, so the second to-arrive blocks or errors instead of corrupting.
- Pin current behavior even if not yet fixed — any future regression away from "deterministic" must trip this test.

---

## P2.29 — TC-ADV: Non-blocking HNSW rebuild — no panic/dim-drift/store-fail tests

**Finding:** P2.29 in audit-triage.md
**Files:** `src/cli/watch.rs:965-1042` (`spawn_hnsw_rebuild`), `:1058+` (`drain_pending_rebuild`); existing tests at `:3979-4115`

### Test skeleton

```rust
#[cfg(test)]
mod rebuild_adversarial_tests {
    use super::*;

    #[test]
    fn test_spawn_hnsw_rebuild_dim_mismatch_clears_pending() {
        // Set up a Store with dim=768, call spawn_hnsw_rebuild with expected_dim=1024,
        // then drain_pending_rebuild; assert pending is cleared and no dangling channel.
    }

    #[test]
    fn test_spawn_hnsw_rebuild_thread_panic_drops_delta_loudly() {
        // Wrap the rebuild closure with a feature-flagged panic injection point.
        // Assert the delta is NOT silently dropped (current behavior leaks it).
        // Production fix wraps closure in catch_unwind and replays delta on panic.
    }

    #[test]
    fn test_spawn_hnsw_rebuild_store_open_fails_clears_pending() {
        // Point at a non-existent index path; assert drain_pending_rebuild
        // clears pending after the receiver sees the error.
    }

    #[test]
    fn test_spawn_hnsw_rebuild_failure_to_spawn_disconnect_path() {
        // Synthesize spawn failure (rlimit on threads); assert a follow-up
        // drain_pending_rebuild clears via Disconnected (does not leak forever).
    }
}
```

### Notes

- The "delta dropped on panic" case is a real bug per the finding, not just a coverage gap. Test must fail today.
- Production fix: wrap the spawn closure in `std::panic::catch_unwind` and replay the delta into `state.hnsw_index` on panic.

---

## P2.30 — TC-ADV: serve auth `strip_token_param` case/percent-encoding gaps

**Finding:** P2.30 in audit-triage.md
**Files:** `src/serve/auth.rs:101-115` (`strip_token_param`); existing tests at `:269-291`

### Test skeleton

```rust
#[cfg(test)]
mod auth_strip_tests {
    use super::*;

    #[test]
    fn test_strip_token_param_case_insensitive() {
        // ?Token=abc — currently kept in URL (capital T fails starts_with("token="))
        // Pin the desired behavior: stripped (auth check should be case-insensitive
        // on param name to match HTTP convention).
        let stripped = strip_token_param("foo=1&Token=secret&bar=2");
        assert!(!stripped.contains("Token"), "case-insensitive strip required");
    }

    #[test]
    fn test_check_request_rejects_percent_encoded_token_key() {
        // ?%74oken=abc (where %74 = 't')
        // Today: literal starts_with("token=") fails, falls through to no-token, 401.
        // Pin: either decode (preferred) or 401.
    }

    #[test]
    fn test_strip_token_param_handles_double_ampersand() {
        // ?token=abc&&depth=3 — the empty pair between && fails starts_with
        // and survives into the rejoined query. Pin redirect output.
        let stripped = strip_token_param("token=abc&&depth=3");
        assert_eq!(stripped, "depth=3");
    }

    #[test]
    fn test_strip_token_param_empty_value() {
        // ?token= — pin behavior: stripped (so it doesn't sit in URL bar).
        let stripped = strip_token_param("token=&depth=3");
        assert_eq!(stripped, "depth=3");
    }
}
```

### Notes

- Production fix: percent-decode the *key* via `percent_encoding::percent_decode_str` (already in dep tree) and lowercase the key. Token *value* stays exact-match for `ct_eq`.
- Failing tests today are the SEC-7 leakage path (token survives in URL bar after redirect).

---

## P2.31 — TC-ADV: `slot::migrate_legacy` rollback path untested

**Finding:** P2.31 in audit-triage.md
**Files:** `src/slot/mod.rs:511-593` (migration), `:561-582` (rollback loop); tests at `:850+` cover happy-path only

### Test skeleton

```rust
#[cfg(test)]
mod migrate_rollback_tests {
    use super::*;

    #[test]
    fn test_migrate_rollback_on_second_file_failure() {
        // Plant index.db and index.db-wal; make index.db-wal fail to move
        // (e.g. open it with an exclusive flock on Linux, or remove read perms
        // mid-test). Assert:
        //   - rollback restores index.db to .cqs/
        //   - slots/ is fully cleaned up
        //   - next migration call still works (idempotent recovery)
    }

    #[test]
    fn test_migrate_rollback_failure_leaves_loud_signal() {
        // Make rollback ITSELF fail (chmod source dir read-only after first move).
        // Assert migration returns Err(SlotError::Migration(...)) AND there is a
        // single known signal (e.g. .cqs/migration_failed marker) — not silent
        // split state.
    }
}
```

### Notes

- Production fix (per RB-V1.30-4): write a `.cqs/migration.lock` sentinel at start, only remove on full success. Subsequent migration calls error if sentinel found.
- EBUSY on Windows for `index.db-wal` is the realistic trigger.

---

## P2.32 — TC-ADV: `LocalProvider` non-HTTP api_base + concurrency mis-sizing

**Finding:** P2.32 in audit-triage.md
**Files:** `src/llm/local.rs:88-121` (`LocalProvider::new`), `:128-312` (`submit_via_chat_completions`), `:153` (channel sizing)

### Test skeleton

```rust
#[cfg(test)]
mod local_provider_edge_tests {
    use super::*;

    #[test]
    fn test_non_http_api_base_fails_fast() {
        let cfg = make_config("file:///tmp/foo", Duration::from_secs(5));
        let provider = LocalProvider::new(&cfg).unwrap();
        let start = std::time::Instant::now();
        let result = provider.submit_batch_prebuilt(&[/* one item */], None);
        // Should error within 100ms, NOT take 7.5s for the full retry stall.
        assert!(result.is_err());
        assert!(start.elapsed() < Duration::from_millis(500));
    }

    #[test]
    fn test_api_base_with_trailing_slash_works() {
        // Pin behavior: ?token=abc with cfg api_base ending in `/`
        // either succeeds (most servers tolerate doubled slash) or normalizes.
    }

    #[test]
    fn test_concurrency_clamped_to_item_count_when_smaller() {
        // For items.len()=1 and concurrency=64, only 1 worker thread
        // should be spawned. Verify via a counter in a custom worker-spawn hook,
        // or by enumerating thread names.
    }
}
```

### Notes

- Production fix: bail in `LocalProvider::new` if `Url::parse(&api_base).scheme() not in {"http", "https"}`. Clamp workers via `let workers = self.concurrency.min(items.len()).max(1);` at line 166.

---

## P2.33 — RB: Slot pointer files unbounded `read_to_string`

**Finding:** P2.33 in audit-triage.md
**Files:** `src/slot/mod.rs:207, 323`

### Current code (representative — :207)

```rust
let raw = match fs::read_to_string(&path) {
    Ok(s) => s,
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
    Err(e) => { /* warn, return None */ }
};
```

### Replacement

```rust
use std::io::Read;

let raw = match std::fs::File::open(&path) {
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
    Err(e) => { tracing::warn!(path = %path.display(), error = %e, "open failed"); return None; }
    Ok(f) => {
        let mut buf = String::new();
        if let Err(e) = f.take(4096).read_to_string(&mut buf) {
            tracing::warn!(path = %path.display(), error = %e, "bounded read failed; treating as missing");
            return None;
        }
        buf
    }
};
```

Apply at both sites (`:207` `read_slot_model` and `:323` `read_active_slot`).

### Notes

- 4 KiB is enough for `slot.toml` (and 100× headroom on the active_slot pointer).
- An oversize pointer file becomes "treated as missing" with a `tracing::warn!`, instead of OOMing every CLI invocation.

---

## P2.34 — RB: `migrate_legacy` rollback leaves undetectable half-state

**Finding:** P2.34 in audit-triage.md
**Files:** `src/slot/mod.rs:511-593`, `:628-638` (`move_file`)

### Replacement

Write a sentinel file before the migration starts; remove on full success only. On startup, refuse if sentinel exists:

```rust
let sentinel = project_cqs_dir.join("migration.lock");
if sentinel.exists() {
    return Err(SlotError::Migration(format!(
        "previous migration failed (see {}). Manually recover then `rm {}`",
        sentinel.display(),
        sentinel.display()
    )));
}
std::fs::write(&sentinel, format!("started_at={}\n", chrono::Utc::now().to_rfc3339()))?;
// ... do the migration (existing logic)
// On full success only:
std::fs::remove_file(&sentinel)?;
```

On the rollback path, leave the sentinel in place but write the failure reason to it:

```rust
// Inside rollback failure arm:
let _ = std::fs::write(&sentinel, format!(
    "failed_at={}\nrollback_failure={}\nrolled_back_files={:?}\n",
    chrono::Utc::now().to_rfc3339(), e, rolled_back
));
```

### Notes

- Pairs with P2.31 test skeleton (rollback failure leaves a loud signal).
- Half-state ambiguity is the actual robustness gap — the sentinel file disambiguates.

---

## P2.35 — RB: `auth_attempts` / `auth_failures` mutex unwrap cascades worker poison

**Finding:** P2.35 in audit-triage.md
**Files:** `src/llm/local.rs:393-396`

### Current code

```rust
if is_first_attempt && (status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN) {
    *auth_attempts.lock().unwrap() += 1;
    *auth_failures.lock().unwrap() += 1;
} else if is_first_attempt {
    *auth_attempts.lock().unwrap() += 1;
}
```

### Replacement

```rust
if is_first_attempt && (status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN) {
    *auth_attempts.lock().unwrap_or_else(|p| p.into_inner()) += 1;
    *auth_failures.lock().unwrap_or_else(|p| p.into_inner()) += 1;
} else if is_first_attempt {
    *auth_attempts.lock().unwrap_or_else(|p| p.into_inner()) += 1;
}
```

### Notes

- Counters are advisory — a poisoned mutex shouldn't escalate to a panic cascade across 64 worker threads.
- Same pattern used elsewhere in P1.9 (LocalProvider mutex poison fix).

---

## P2.36 — RB: redirect policy disagrees between production (none) and doctor (limited(2))

**Finding:** P2.36 in audit-triage.md
**Files:** `src/llm/local.rs:99` (`Policy::none()`) vs `src/cli/commands/infra/doctor.rs:578` (`Policy::limited(2)`)

### Current code

```rust
// src/llm/local.rs:97-100
let http = Client::builder()
    .timeout(timeout)
    .redirect(reqwest::redirect::Policy::none())
    .build()?;

// src/cli/commands/infra/doctor.rs:576-580
let client = match reqwest::blocking::Client::builder()
    .timeout(std::time::Duration::from_secs(3))
    .redirect(reqwest::redirect::Policy::limited(2))
    .build()
```

### Replacement

Align both to `Policy::limited(2)` (a same-origin HTTP→HTTPS redirect on bind-localhost is benign):

```rust
// src/llm/local.rs:99
.redirect(reqwest::redirect::Policy::limited(2))
```

### Notes

- Alternative: keep `Policy::none()` in production but log a once-per-launch warning in doctor when a redirect was followed during the probe. Less surgical but preserves strict prod stance.

---

## P2.37 — SHL: CAGRA `itopk_size < k` on small indexes

**Finding:** P2.37 in audit-triage.md
**Files:** `src/cagra.rs:359` (computation), `:166-170` (`cagra_itopk_max_default`)

### Current code

```rust
let itopk_size = (k * 2).clamp(itopk_min, itopk_max);
```

### Replacement

Enforce the cuVS hard requirement `itopk_size >= k`, and degrade if the cap can't honor it:

```rust
// CONSTRAINT: cuVS CAGRA requires itopk_size >= k. Document at top of fn.
let itopk_size = (k * 2).clamp(itopk_min, itopk_max).max(k);
if itopk_size > itopk_max {
    tracing::warn!(
        k,
        itopk_max,
        n_vectors = self.len(),
        "CAGRA: k exceeds itopk_max for this corpus size; falling back to HNSW"
    );
    return Err(CagraError::CapacityExceeded { k, itopk_max });
}
```

Add `CagraError::CapacityExceeded { k: usize, itopk_max: usize }` variant.

### Notes

- The `MEMORY.md` workaround "keep eval at limit=20" only protects the eval path; production `cqs search --limit 500 --rerank` over a small subset trips this silently.
- Caller in `src/store/search.rs` must catch `CapacityExceeded` and fall back to HNSW cleanly.

---

## P2.38 — SHL: `nl::generate_nl` `char_budget` defaults to 512 even with 2048 max_seq_len

**Finding:** P2.38 in audit-triage.md
**Files:** `src/nl/mod.rs:222-229`

### Current code

```rust
static MAX_SEQ: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
let max_seq = *MAX_SEQ.get_or_init(|| {
    std::env::var("CQS_MAX_SEQ_LENGTH")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(512)
});
```

### Replacement

Plumb the active model's `max_seq_length` through. Pass it as an argument to `generate_nl_with_template`:

```rust
pub(crate) fn generate_nl_with_template(
    chunk: &Chunk,
    template: NlTemplate,
    model_max_seq_len: usize,  // NEW: from caller's ModelConfig
) -> String {
    // ...
    // Env var becomes a fallback override only:
    let max_seq = std::env::var("CQS_MAX_SEQ_LENGTH")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(model_max_seq_len);
    let char_budget = max_seq.saturating_mul(4).saturating_sub(200).max(400);
    // ...
}
```

Caller in `pipeline/parsing.rs` already has `Embedder` in scope; pass `embedder.model_config().max_seq_length`.

### Notes

- BGE/E5/v9-200k all use 512; **nomic-coderank uses 2048** per `embedder/models.rs:366` — env var as source of truth caps it at 25% of capacity.
- The OnceLock memoization no longer makes sense once the value depends on the caller; drop it.

---

## P2.39 — SHL: `MAX_BATCH_SIZE = 10_000` LLM module

**Finding:** P2.39 in audit-triage.md
**Files:** `src/llm/mod.rs:192`; consumers at `src/llm/summary.rs:58,92`, `src/llm/hyde.rs:39-41`, `src/llm/doc_comments.rs:271`

### Current code

```rust
const MAX_BATCH_SIZE: usize = 10_000;
```

### Replacement

Move to `src/limits.rs` with an env resolver:

```rust
// src/limits.rs
pub fn llm_max_batch_size() -> usize {
    parse_env_usize_clamped("CQS_LLM_MAX_BATCH_SIZE", 10_000, 1, 100_000)
}
```

Replace the `const MAX_BATCH_SIZE` import with a function call at every consumer site. At CLI exit, when truncation triggered, surface a hint (current `tracing::info!` is invisible without `RUST_LOG=info`):

```rust
if remaining_chunks > 0 {
    eprintln!("note: {} chunks remain unprocessed (cap CQS_LLM_MAX_BATCH_SIZE={}). Rerun to continue.",
        remaining_chunks, llm_max_batch_size());
}
```

### Notes

- 100,000 hard cap honors Anthropic's actual Batches API limit.
- HyDE and doc_comments share the same const — one env var covers both, but consider `CQS_LLM_HYDE_MAX_BATCH_SIZE` if cost characteristics diverge enough.

---

## P2.40 — SHL: serve `ABS_MAX_GRAPH_NODES`/`ABS_MAX_CLUSTER_NODES` 50k hardcoded

**Finding:** P2.40 in audit-triage.md
**Files:** `src/serve/data.rs:17,24,505,542,571`

### Current code

```rust
pub(crate) const ABS_MAX_GRAPH_NODES: usize = 50_000;
pub(crate) const ABS_MAX_GRAPH_EDGES: usize = 500_000;
pub(crate) const ABS_MAX_CLUSTER_NODES: usize = 50_000;
// ... in build_chunk_detail:
// :505 LIMIT 50 callers
// :542 LIMIT 50 callees
// :571 LIMIT 20 tests
```

### Replacement

Move to `src/limits.rs` with env overrides + a hard ceiling:

```rust
pub fn serve_max_graph_nodes() -> usize {
    parse_env_usize_clamped("CQS_SERVE_MAX_GRAPH_NODES", 50_000, 1, 1_000_000)
}
pub fn serve_max_cluster_nodes() -> usize { /* analog */ }
pub fn serve_chunk_detail_callers_limit() -> usize {
    parse_env_usize_clamped("CQS_SERVE_CHUNK_DETAIL_CALLERS", 50, 1, 1_000)
}
// ... callees, tests
```

In `build_chunk_detail`, bind the limits as `?` parameters and emit `truncated: bool` when the cap is hit. Accept `?max_callers / ?max_callees / ?max_tests` query params.

### Notes

- Cytoscape's render ceiling is ~5-10k nodes anyway, so the *default* 50k is too high for the UI; the *cap* 1M is for power-user queries.
- For graph/cluster, derive the default from `chunk_count` so small projects ship the whole graph.

---

## P2.41 — SHL: `embed_batch_size` default 64 doesn't scale with model dim/seq

**Finding:** P2.41 in audit-triage.md
**Files:** `src/cli/pipeline/types.rs:143-160`, `src/embedder/mod.rs:685-689`

### Current code

```rust
pub(crate) fn embed_batch_size() -> usize {
    match std::env::var("CQS_EMBED_BATCH_SIZE") {
        Ok(val) => match val.parse::<usize>() {
            Ok(size) if size > 0 => size,
            _ => 64,
        },
        Err(_) => 64,
    }
}
```

### Replacement

Make the default scale with model dim and seq_len:

```rust
pub(crate) fn embed_batch_size_for(model: &cqs::embedder::ModelConfig) -> usize {
    if let Ok(val) = std::env::var("CQS_EMBED_BATCH_SIZE") {
        if let Ok(size) = val.parse::<usize>() { if size > 0 { return size; } }
    }
    // Target ~130 MB per forward-pass tensor:
    //   batch * seq * dim * 4 bytes
    // With BGE-large (1024 dim, 512 seq): 64 * 512 * 1024 * 4 ≈ 130 MB
    // Scale inversely as dim or seq grow.
    let baseline = 64.0_f64;
    let dim_factor = 1024.0 / model.dim as f64;
    let seq_factor = (512.0 / model.max_seq_length as f64).max(0.25);
    let scaled = (baseline * dim_factor * seq_factor).max(1.0) as usize;
    // Round to nearest power of 2 for ORT efficiency
    scaled.next_power_of_two().min(256).max(2)
}
```

Replace `embed_batch_size()` callers with `embed_batch_size_for(&self.model_config)`.

### Notes

- BGE-large + 512 seq + 64 batch = OK on RTX 4060 8GB. Nomic-coderank + 2048 seq + 64 batch OOMs.
- Optional follow-on: query GPU VRAM via `nvml-wrapper` (transitive via cuVS) and target 25% of free VRAM.

---

## P2.42 — SHL: `CagraIndex::gpu_available` no VRAM ceiling — OOMs on 8GB GPUs

**Finding:** P2.42 in audit-triage.md
**Files:** `src/cagra.rs:262-264`

### Current code

```rust
pub fn gpu_available() -> bool {
    cuvs::Resources::new().is_ok()
}
```

### Replacement

Probe GPU VRAM and gate on estimated build memory:

```rust
pub fn gpu_available_for(n_vectors: usize, dim: usize) -> bool {
    if cuvs::Resources::new().is_err() {
        return false;
    }
    // Estimate build memory: dataset bytes + graph bytes + ~30% slack
    let dataset_bytes = (n_vectors * dim * 4) as u64;
    let graph_bytes = (n_vectors * 64 * 4) as u64; // graph_degree default 64
    let estimated = (dataset_bytes + graph_bytes) * 130 / 100;

    let cap = std::env::var("CQS_CAGRA_MAX_GPU_BYTES")
        .ok().and_then(|s| s.parse::<u64>().ok())
        .unwrap_or_else(|| {
            // Best-effort: query free VRAM. If we cannot, fall back to a
            // conservative 2 GiB cap so 8 GiB GPUs don't OOM.
            cuvs_free_vram_bytes().unwrap_or(2 * 1024 * 1024 * 1024)
        });
    if estimated > cap * 80 / 100 {
        tracing::warn!(estimated, cap, "GPU has insufficient free VRAM for CAGRA build — falling back to HNSW");
        return false;
    }
    true
}

// Back-compat shim:
pub fn gpu_available() -> bool { Self::gpu_available_for(0, 0) }
```

Caller in `cli/store.rs::build_vector_index_with_config` switches to `gpu_available_for(chunk_count, dim)`.

### Notes

- `cuvs_free_vram_bytes()` may need to call CUDA's `cudaMemGetInfo` directly via FFI if `cuvs` doesn't expose it. Investigate before signing off.
- A6000 48GB hosts are unaffected; RTX 4000 8GB benefits.

---

## P2.43 — `semantic_diff` sort tie-breaker (already fixed — pin with test)

**Finding:** P2.43 in audit-triage.md
**Files:** `src/diff.rs:202-218` — fix already on disk (cascade on `(file, name, chunk_type)`).

### Regression-pin test

```rust
#[cfg(test)]
mod determinism_tests {
    use super::*;

    #[test]
    fn semantic_diff_sort_is_deterministic_under_shuffled_input() {
        // Build a DiffResult with 5 modified entries, all similarity=0.73
        // and varying (file, name) tuples.
        let entries = vec![
            DiffEntry { file: "z.rs".into(), name: "a".into(), similarity: Some(0.73), chunk_type: ChunkType::Function, /* ... */ },
            DiffEntry { file: "a.rs".into(), name: "z".into(), similarity: Some(0.73), chunk_type: ChunkType::Function, /* ... */ },
            // ... 3 more
        ];
        let mut runs = Vec::new();
        for _ in 0..50 {
            let mut shuffled = entries.clone();
            // Use a different random seed each iteration
            shuffled.shuffle(&mut rand::thread_rng());
            // Re-run the production sort
            shuffled.sort_by(/* the cascade from src/diff.rs:208-218 */);
            runs.push(shuffled);
        }
        // Assert all 50 produced the same order
        for w in runs.windows(2) {
            assert_eq!(w[0], w[1], "sort must be deterministic across input shuffles");
        }
    }
}
```

### Notes

- Production code at `src/diff.rs:208-218` already cascades — this test pins it so a future regression is loud.

---

## P2.44 — `is_structural_query` end-of-query keyword (already fixed — pin with test)

**Finding:** P2.44 in audit-triage.md
**Files:** `src/search/router.rs:806-817` — fix already on disk (uses `words.iter().any(|w| w == kw)` instead of `format!(" {} ", kw)`).

### Regression-pin test

```rust
#[cfg(test)]
mod structural_query_tests {
    use super::*;

    #[test]
    fn structural_keywords_at_end_of_query_route_correctly() {
        // Each of these ends with a structural keyword and must be is_structural=true
        for q in &["find all trait", "show me all trait", "find every impl", "list all enum", "all class", "find enum"] {
            assert!(is_structural_query(q), "query `{}` must classify as structural", q);
        }
    }

    #[test]
    fn structural_keyword_as_substring_does_not_falsely_match() {
        // "training" contains "trait" but NOT as a word — must NOT classify structural
        assert!(!is_structural_query("training pipeline"));
    }
}
```

### Notes

- Production code is correct; this is regression protection.

---

## P2.45 — `bfs_expand` HashMap seed order (already fixed — pin with test)

**Finding:** P2.45 in audit-triage.md
**Files:** `src/gather.rs:317-330` — fix already on disk (sorts seeds by `(score desc, name asc)` before enqueue).

### Regression-pin test

```rust
#[cfg(test)]
mod bfs_seed_order_tests {
    use super::*;

    #[test]
    fn bfs_expand_is_deterministic_under_seed_shuffling() {
        // Build name_scores with two entries scored equally and one above.
        // Run bfs_expand 100 times; assert the resulting name_scores is
        // identical across runs.
    }

    #[test]
    fn bfs_expand_processes_higher_scoring_seed_first() {
        // Two seeds: ("foo", 0.9) and ("bar", 0.5).
        // With max_expanded_nodes=1 (cap before second seed expands),
        // assert "foo"'s neighbors got into name_scores, not "bar"'s.
    }
}
```

### Notes

- Production code is correct; this pins it.

---

## P2.46 — `contrastive_neighbors` top-K tie-break (already fixed — pin with test)

**Finding:** P2.46 in audit-triage.md
**Files:** `src/llm/summary.rs:263-282` — fix already on disk (`.then(a.0.cmp(&b.0))` cascade on all three sorts).

### Regression-pin test

```rust
#[cfg(test)]
mod contrastive_neighbor_tests {
    use super::*;

    #[test]
    fn contrastive_neighbors_top_k_is_deterministic_under_ties() {
        // Build a similarity matrix where row 0 has multiple entries scoring
        // exactly the same. Run contrastive_neighbors 50 times; assert the
        // returned neighbor list is identical every time.
    }
}
```

### Notes

- Production code is correct; this pins it for the cache-cost-sensitive contrastive summary path (~$0.38/run Haiku regenerates if cache misses).

---
