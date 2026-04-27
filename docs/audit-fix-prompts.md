# v1.30.0 Audit — P1 Fix Prompts

21 fix prompts for the P1 (easy + high-impact) findings. Verified against current source 2026-04-26.

Notes on drift:
- P1.16 — `--name-boost` CLI parser is already `parse_unit_f32` (clap-bounded). The audit text was stale; only the **defense-in-depth** clamp at the consumer remains.
- All other P1 line citations verified to current source.

---

## P1.1 + P1.2 — PRIVACY/SECURITY lie about query_log opt-in and query_cache TTL

**Finding:** P1.1 + P1.2 in audit-triage.md / Documentation in audit-findings.md (DOC-V1.30-1, DOC-V1.30-2)
**Files:** `PRIVACY.md:21-22`, `SECURITY.md:101`, `src/cli/batch/commands.rs:371-399`
**Why:** PRIVACY claims query_log is opt-in but `log_query` is unconditional. PRIVACY claims 7-day TTL on query_cache; only size cap exists. Both are P1 "docs lying" — fix the docs (defensible direction; gating the log requires touching every dispatch arm and changing user-visible behaviour).

### Current docs (PRIVACY.md:21-22)

```markdown
- `~/.cache/cqs/query_cache.db` — recent query embeddings with a 7-day TTL. Speeds up repeated searches.
- `~/.cache/cqs/query_log.jsonl` — opt-in query log, written only when `CQS_TELEMETRY=1` or the file already exists. Stays local.
```

### Replacement (PRIVACY.md:21-22)

```markdown
- `~/.cache/cqs/query_cache.db` — recent query embeddings, evicted oldest-first when the DB exceeds `CQS_QUERY_CACHE_MAX_SIZE` (100 MiB default). Prune older entries with `cqs cache prune <DAYS>`. Speeds up repeated searches.
- `~/.cache/cqs/query_log.jsonl` — local query log written by every `cqs chat` / `cqs batch` invocation (search/gather/onboard/scout/where/task). Append-only JSONL. Delete the file to disable; `cqs cache clear` does not remove it. Stays local.
```

### Current docs (SECURITY.md:101)

```markdown
| `~/.cache/cqs/query_log.jsonl` | Opt-in local query log | `CQS_TELEMETRY=1` or file exists |
```

### Replacement (SECURITY.md:101)

```markdown
| `~/.cache/cqs/query_log.jsonl` | Local query log (append-only) | `cqs chat` / `cqs batch` (search, gather, onboard, scout, where, task) |
```

### Notes

- The "fix the code" alternative (gate `log_query` on `CQS_TELEMETRY=1` plus existing-file check) is the *more defensible* contract but a behaviour change. Pick docs-fix here unless reviewer explicitly opts into a behaviour change. If the reviewer chooses code-fix, gate the body of `log_query` (`src/cli/batch/commands.rs:371`) on `std::env::var("CQS_TELEMETRY").as_deref() == Ok("1") || log_path.exists()` returning early otherwise.

---

## P1.3 — CHANGELOG names CQS_LLM_ENDPOINT — actual var is CQS_LLM_API_BASE

**Finding:** P1.3 in audit-triage.md / Documentation DOC-V1.30-3
**Files:** `CHANGELOG.md:19`
**Why:** CHANGELOG v1.30.0 entry references `CQS_LLM_ENDPOINT`, which does not exist in the codebase. Actual env var is `CQS_LLM_API_BASE` (plus `CQS_LLM_PROVIDER=local`). User copy-pasting fails at runtime.

### Current

```markdown
- **Local LLM provider (OpenAI-compatible)** — `cqs index --llm-summaries` accepts a local OpenAI-compatible endpoint via `CQS_LLM_ENDPOINT`, in addition to the existing Anthropic Batches API path (#1101 — closes audit finding EX-V1.29-3). `LlmProvider` trait, `LocalProvider` implementation, end-to-end summary generation via local vLLM / Ollama / etc.
```

### Replacement

```markdown
- **Local LLM provider (OpenAI-compatible)** — `cqs index --llm-summaries` accepts a local OpenAI-compatible endpoint via `CQS_LLM_PROVIDER=local` + `CQS_LLM_API_BASE=<url>`, in addition to the existing Anthropic Batches API path (#1101 — closes audit finding EX-V1.29-3). `LlmProvider` trait, `LocalProvider` implementation, end-to-end summary generation via local vLLM / Ollama / etc.
```

---

## P1.4 — CONTRIBUTING tells contributors to edit dispatch.rs (registry.rs now)

**Finding:** P1.4 in audit-triage.md / Documentation DOC-V1.30-4
**Files:** `CONTRIBUTING.md:339-355` (checklist), `CONTRIBUTING.md:153-158` (Architecture Overview)
**Why:** v1.30.0 #1097/#1114 collapsed five exhaustive matches in `dispatch.rs`/`definitions.rs` into one `for_each_command!` table in `src/cli/registry.rs`. The contributor checklist still says "match arm in `src/cli/dispatch.rs`" with no mention of `registry.rs`.

### Current (CONTRIBUTING.md:339-355)

```markdown
## Adding a New CLI Command

Checklist for every new command:

1. **Implementation** — `src/cli/commands/<category>/<name>.rs` with the core logic (pick category: search/, graph/, review/, index/, io/, infra/, train/)
2. **Category mod.rs** — add `mod <name>;` + `pub(crate) use <name>::*;` in `src/cli/commands/<category>/mod.rs`
3. **CLI definition** — `Commands` enum variant in `src/cli/definitions.rs` with clap args
4. **Dispatch** — match arm in `src/cli/dispatch.rs`
5. **`--json` support** — serde serialization for programmatic output
6. **Tracing** — `tracing::info_span!` at entry, `tracing::warn!` on error fallback
7. **Error handling** — `Result` propagation, no bare `.unwrap_or_default()` in production
8. **Tests** — happy path + empty input + error path + edge cases
9. **CLAUDE.md** — add to the command reference section
10. **Skills** — add to `.claude/skills/cqs/SKILL.md` and `.claude/skills/cqs-bootstrap/SKILL.md`
11. **CHANGELOG** — entry in the next release section
```

### Replacement (CONTRIBUTING.md:339-355)

```markdown
## Adding a New CLI Command

Checklist for every new command:

1. **Implementation** — `src/cli/commands/<category>/<name>.rs` with the core logic (pick category: search/, graph/, review/, index/, io/, infra/, train/)
2. **Category mod.rs** — add `mod <name>;` + `pub(crate) use <name>::*;` in `src/cli/commands/<category>/mod.rs`
3. **CLI definition** — `Commands` enum variant in `src/cli/definitions.rs` with clap args
4. **Registry row** — add a `(bind, wild, name, batch_support, body)` row to `group_a` or `group_b` in `src/cli/registry.rs`. The `for_each_command!` macro generates dispatch + variant_name + batch_support from this single row; a missing row is a compile error.
5. **`--json` support** — serde serialization for programmatic output
6. **Tracing** — `tracing::info_span!` at entry, `tracing::warn!` on error fallback
7. **Error handling** — `Result` propagation, no bare `.unwrap_or_default()` in production
8. **Tests** — happy path + empty input + error path + edge cases
9. **CLAUDE.md** — add to the command reference section
10. **Skills** — add to `.claude/skills/cqs/SKILL.md` and `.claude/skills/cqs-bootstrap/SKILL.md`
11. **CHANGELOG** — entry in the next release section
```

### Current (Architecture Overview, CONTRIBUTING.md:153-158)

```markdown
  cli/          - Command-line interface (clap)
    mod.rs      - Top-level CLI module, re-exports
    definitions.rs - Clap argument definitions and command enum
    dispatch.rs - Command dispatch (match on command, call handlers)
    commands/   - Command implementations (organized by category)
```

### Replacement (Architecture Overview)

```markdown
  cli/          - Command-line interface (clap)
    mod.rs      - Top-level CLI module, re-exports
    definitions.rs - Clap argument definitions and command enum
    registry.rs - `for_each_command!` table; single source of truth for dispatch + variant_name + batch_support
    dispatch.rs - Command dispatch helpers (entry points; per-command arms generated from `registry.rs`)
    commands/   - Command implementations (organized by category)
```

---

## P1.5 — ProjectRegistry doc lies about path on macOS/Windows

**Finding:** P1.5 in audit-triage.md / Platform Behavior section
**Files:** `src/project.rs:1-3, 176-179`
**Why:** Module-level doc claims `~/.config/cqs/projects.toml` but `dirs::config_dir()` returns macOS-specific (`~/Library/Application Support/`) and Windows-specific (`%APPDATA%\`) paths. Path is constructed correctly; only the doc is wrong.

### Current code (src/project.rs:1-4)

```rust
//! Cross-project search via global project registry.
//!
//! Maintains a registry of indexed projects at `~/.config/cqs/projects.toml`.
//! Enables searching across all registered projects from anywhere.
```

### Replacement

```rust
//! Cross-project search via global project registry.
//!
//! Maintains a registry of indexed projects in the platform config directory
//! (via `dirs::config_dir()`):
//! - Linux: `~/.config/cqs/projects.toml`
//! - macOS: `~/Library/Application Support/cqs/projects.toml`
//! - Windows: `%APPDATA%\cqs\projects.toml`
//!
//! Enables searching across all registered projects from anywhere.
```

### Current code (src/project.rs:175-179)

```rust
/// Get the registry file path
fn registry_path() -> Result<PathBuf, ProjectError> {
    let config_dir = dirs::config_dir().ok_or(ProjectError::ConfigDirNotFound)?;
    Ok(config_dir.join("cqs").join("projects.toml"))
}
```

### Replacement

```rust
/// Get the registry file path.
///
/// Resolves via `dirs::config_dir()`:
/// - Linux: `~/.config/cqs/projects.toml`
/// - macOS: `~/Library/Application Support/cqs/projects.toml`
/// - Windows: `%APPDATA%\cqs\projects.toml`
fn registry_path() -> Result<PathBuf, ProjectError> {
    let config_dir = dirs::config_dir().ok_or(ProjectError::ConfigDirNotFound)?;
    Ok(config_dir.join("cqs").join("projects.toml"))
}
```

---

## P1.6 — gather warning hardcodes "200" — lies when CQS_GATHER_MAX_NODES set

**Finding:** P1.6 in audit-triage.md / Code Quality
**Files:** `src/cli/commands/search/gather.rs:200`, `src/gather.rs:153-172` (`gather_max_nodes`)
**Why:** Text-mode warning says "capped at 200 nodes" but actual cap obeys `CQS_GATHER_MAX_NODES`. With `CQS_GATHER_MAX_NODES=500`, the user sees "capped at 200" while results were capped at 500.

### Current code (src/cli/commands/search/gather.rs:199-201)

```rust
        if result.expansion_capped {
            println!("{}", "Warning: expansion capped at 200 nodes".yellow());
        }
```

### Replacement

```rust
        if result.expansion_capped {
            let cap = cqs::gather::gather_max_nodes();
            println!(
                "{}",
                format!("Warning: expansion capped at {cap} nodes").yellow()
            );
        }
```

### Notes

- `gather_max_nodes` is already memoized via `OnceLock` (per `src/gather.rs:153-172`), so this is cheap.
- Verify that `gather::gather_max_nodes` is `pub`; if not, expose it (`pub fn`) and re-export from `lib.rs` if needed.
- Stretch goal flagged in audit: surface `expansion_cap_used: usize` on `GatherResult` so `--json` consumers also see the real cap. Out of scope for the easy fix here.

---

## P1.7 — Reranker silently ignores [reranker] config section

**Finding:** P1.7 in audit-triage.md / Code Quality
**Files:** `src/reranker.rs:127-154` (`Reranker::new`), `src/reranker.rs:61-77` (`resolve_reranker`), `src/reranker.rs:442-446` (`model_paths` calls `resolve_reranker(None)`), production callers `src/cli/store.rs:276`, `src/cli/batch/mod.rs:1274`
**Why:** `resolve_reranker` accepts `Option<&AuxModelSection>` to thread `Config::reranker` (`src/config.rs:228`) through, but `Reranker::model_paths` always passes `None`. A user `.cqs.toml` `[reranker] preset = "bge-reranker-base"` is silently dropped — user gets default ms-marco-MiniLM with no error or warn.

### Current code (src/reranker.rs:106-154)

```rust
pub struct Reranker {
    session: Mutex<Option<Session>>,
    tokenizer: Mutex<Option<Arc<tokenizers::Tokenizer>>>,
    model_paths: OnceCell<(PathBuf, PathBuf)>,
    provider: ExecutionProvider,
    max_length: usize,
    expects_token_type_ids: Mutex<Option<bool>>,
}

impl Reranker {
    /// Create a new reranker with lazy model loading
    pub fn new() -> Result<Self, RerankerError> {
        let provider = select_provider();
        let max_length = match std::env::var("CQS_RERANKER_MAX_LENGTH") {
            // ... unchanged ...
        };
        Ok(Self {
            session: Mutex::new(None),
            tokenizer: Mutex::new(None),
            model_paths: OnceCell::new(),
            provider,
            max_length,
            expects_token_type_ids: Mutex::new(None),
        })
    }
```

### Current code (src/reranker.rs:442-446)

```rust
    fn model_paths(&self) -> Result<&(PathBuf, PathBuf), RerankerError> {
        self.model_paths.get_or_try_init(|| {
            let _span = tracing::info_span!("reranker_model_resolve").entered();

            let resolved = resolve_reranker(None)?;
```

### Replacement (struct + constructors, src/reranker.rs:106-154)

```rust
pub struct Reranker {
    session: Mutex<Option<Session>>,
    tokenizer: Mutex<Option<Arc<tokenizers::Tokenizer>>>,
    model_paths: OnceCell<(PathBuf, PathBuf)>,
    provider: ExecutionProvider,
    max_length: usize,
    expects_token_type_ids: Mutex<Option<bool>>,
    /// Cached config-file `[reranker]` section so `resolve_reranker` honours
    /// `preset` / `model_path` / `tokenizer_path` set in `.cqs.toml`.
    section: Option<AuxModelSection>,
}

impl Reranker {
    /// Create a new reranker with lazy model loading (config-less; CLI/env only).
    pub fn new() -> Result<Self, RerankerError> {
        Self::with_section(None)
    }

    /// Create a reranker, threading a `[reranker]` config section through to
    /// `resolve_reranker` so `.cqs.toml` preset / model_path are honoured.
    pub fn with_section(section: Option<AuxModelSection>) -> Result<Self, RerankerError> {
        let provider = select_provider();
        let max_length = match std::env::var("CQS_RERANKER_MAX_LENGTH") {
            // ... unchanged body ...
        };
        Ok(Self {
            session: Mutex::new(None),
            tokenizer: Mutex::new(None),
            model_paths: OnceCell::new(),
            provider,
            max_length,
            expects_token_type_ids: Mutex::new(None),
            section,
        })
    }
```

### Replacement (model_paths, src/reranker.rs:442-446)

```rust
    fn model_paths(&self) -> Result<&(PathBuf, PathBuf), RerankerError> {
        self.model_paths.get_or_try_init(|| {
            let _span = tracing::info_span!("reranker_model_resolve").entered();

            let resolved = resolve_reranker(self.section.as_ref())?;
```

### Replacement (callers)

`src/cli/store.rs:276`:
```rust
        let r = cqs::Reranker::with_section(config.reranker.clone())
            .map_err(|e| anyhow::anyhow!("Reranker init failed: {e}"))?;
```

`src/cli/batch/mod.rs:1274`:
```rust
        let r = cqs::Reranker::with_section(config.reranker.clone())
            .map_err(|e| anyhow::anyhow!("Reranker init failed: {e}"))?;
```

### Notes

- `AuxModelSection` is owned (`Clone`-able) per `src/config.rs:228`; cloning is cheap (small struct of `Option<String>`s).
- Need `pub use` of `AuxModelSection` if not already exported from `cqs::` lib; check `src/lib.rs`.
- Re-export verify: `cqs::Reranker::with_section` must compile from outside the crate root (`cli` is in-crate, fine).
- Test sites in `src/reranker.rs:712, 718, 772, 823` continue using `Reranker::new()` (no section needed).
- Add a regression test `with_section_preset_overrides_default` that constructs a `Reranker::with_section(Some(AuxModelSection { preset: Some("bge-reranker-base".into()), .. }))`, calls `model_paths()`, and asserts the resolved repo matches the bge preset (not ms-marco).

---

## P1.8 — Embedder fingerprint falls back to repo:timestamp — cache thrash

**Finding:** P1.8 in audit-triage.md / Error Handling + Data Safety
**Files:** `src/embedder/mod.rs:435-466`
**Why:** Three error arms fall back to `format!("{}:{}", self.model_config.repo, ts)` where `ts = SystemTime::now()`. Every restart with a transient hash failure (AV scanner, EBUSY, I/O hiccup) writes cache rows under a NEW timestamp, so subsequent reads miss the cache forever. Cross-slot copy by `content_hash` also breaks because the model fingerprint isn't stable.

### Current code (src/embedder/mod.rs:432-466)

```rust
                                            hash
                                        }
                                        Err(e) => {
                                            tracing::warn!(error = %e, "Failed to stream-hash model, using repo+timestamp fallback");
                                            let ts = std::time::SystemTime::now()
                                                .duration_since(std::time::UNIX_EPOCH)
                                                .unwrap_or_default()
                                                .as_secs();
                                            format!("{}:{}", self.model_config.repo, ts)
                                        }
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!(error = %e, "Failed to open model for fingerprint, using repo+timestamp fallback");
                                    let ts = std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .unwrap_or_default()
                                        .as_secs();
                                    format!("{}:{}", self.model_config.repo, ts)
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to get model paths for fingerprint, using repo+timestamp fallback");
                    let ts = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    format!("{}:{}", self.model_config.repo, ts)
                }
            }
        })
    }
```

### Replacement

Replace each `repo:timestamp` fallback with a stable `repo:fallback:size=<bytes>` (or `:size=unknown`) shape. The size+repo proxy is deterministic across restarts within the same model file.

```rust
                                            hash
                                        }
                                        Err(e) => {
                                            tracing::warn!(
                                                error = %e,
                                                "Failed to stream-hash model, using repo+size fallback (cache may miss until next successful hash)"
                                            );
                                            let size = std::fs::metadata(model_path)
                                                .ok()
                                                .map(|m| m.len())
                                                .unwrap_or(0);
                                            format!("{}:fallback:size={}", self.model_config.repo, size)
                                        }
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        error = %e,
                                        "Failed to open model for fingerprint, using repo+size fallback"
                                    );
                                    let size = std::fs::metadata(model_path)
                                        .ok()
                                        .map(|m| m.len())
                                        .unwrap_or(0);
                                    format!("{}:fallback:size={}", self.model_config.repo, size)
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "Failed to get model paths for fingerprint, using repo-only fallback"
                    );
                    format!("{}:fallback:no-path", self.model_config.repo)
                }
            }
        })
    }
```

### Notes

- The "model path resolution failed" arm (line 457) cannot read file size (path unknown), so use `:fallback:no-path` as a stable sentinel — still not time-varying.
- Audit suggests promoting the failure to a hard error. That's a behaviour change with downstream surface (every `Embedder::*` call now returns `Result<&str, EmbedderError>`). Out of scope here; the size-fallback closes the cache-thrash bug without breaking existing call signatures.
- `cqs doctor` should warn when `model_fingerprint` starts with `:fallback:` — separate small follow-up; cite as `audit doctor warn fallback fingerprint`.

---

## P1.9 — LocalProvider Mutex::into_inner().unwrap_or_default() loses all batch results on poison

**Finding:** P1.9 in audit-triage.md / Error Handling
**Files:** `src/llm/local.rs:271, 272, 278, 279, 305, 308`
**Why:** On `Mutex::into_inner` returning `Err` (poison), `unwrap_or_default()` substitutes an empty `HashMap` and `submit_via_chat_completions` reports `succeeded` ≠ map size with no error signal. Downstream `fetch_batch_results` returns `{}` and the user persists nothing.

### Current code (src/llm/local.rs:271-309)

```rust
        let ok = *succeeded.lock().unwrap();
        let err = *failed.lock().unwrap();
        let elapsed_ms = start.elapsed().as_millis() as u64;

        // Fatal-batch check: if every item that talked to the server saw
        // 401/403 on its first request, the credentials are wrong — abort
        // with a specific error instead of silently returning an empty stash.
        let auth_fail = *auth_failures.lock().unwrap();
        let auth_attempt = *auth_attempts.lock().unwrap();
        if auth_attempt > 0 && auth_fail == auth_attempt {
            tracing::error!(
                url = %self.api_base,
                "local batch aborted: all {} requests rejected with 401/403",
                auth_attempt
            );
            return Err(LlmError::Api {
                status: 401,
                message: format!(
                    "Authentication rejected at {}; check CQS_LLM_API_KEY",
                    self.api_base
                ),
            });
        }

        tracing::info!(
            batch_id = %batch_id,
            submitted = items.len(),
            succeeded = ok,
            failed = err,
            elapsed_ms,
            "local batch complete"
        );

        // Move results into the stash under the batch id.
        let results_map = results.into_inner().unwrap_or_default();
        self.stash
            .lock()
            .unwrap()
            .insert(batch_id.clone(), results_map);

        Ok(batch_id)
    }
```

### Replacement

```rust
        // Recover counters even on poison — counts are advisory and dropping
        // the count to 0 would mask real progress in the "complete" log.
        let ok = *succeeded.lock().unwrap_or_else(|p| p.into_inner());
        let err = *failed.lock().unwrap_or_else(|p| p.into_inner());
        let elapsed_ms = start.elapsed().as_millis() as u64;

        // Fatal-batch check: if every item that talked to the server saw
        // 401/403 on its first request, the credentials are wrong — abort
        // with a specific error instead of silently returning an empty stash.
        let auth_fail = *auth_failures.lock().unwrap_or_else(|p| p.into_inner());
        let auth_attempt = *auth_attempts.lock().unwrap_or_else(|p| p.into_inner());
        if auth_attempt > 0 && auth_fail == auth_attempt {
            tracing::error!(
                url = %self.api_base,
                "local batch aborted: all {} requests rejected with 401/403",
                auth_attempt
            );
            return Err(LlmError::Api {
                status: 401,
                message: format!(
                    "Authentication rejected at {}; check CQS_LLM_API_KEY",
                    self.api_base
                ),
            });
        }

        tracing::info!(
            batch_id = %batch_id,
            submitted = items.len(),
            succeeded = ok,
            failed = err,
            elapsed_ms,
            "local batch complete"
        );

        // Move results into the stash under the batch id. On poison we recover
        // the partially-populated map rather than silently substituting an
        // empty one — losing partial results is worse than the panic risk.
        let results_map = match results.into_inner() {
            Ok(m) => m,
            Err(poisoned) => {
                tracing::error!(
                    succeeded = ok,
                    "results mutex poisoned during local batch — recovering inner state"
                );
                poisoned.into_inner()
            }
        };

        // Invariant: if results_map.len() != ok, accounting drifted. Surface
        // it loudly rather than shipping a short stash silently.
        if results_map.len() != ok {
            tracing::error!(
                map_len = results_map.len(),
                succeeded = ok,
                "local batch accounting drift: results map size != succeeded count"
            );
            return Err(LlmError::Internal(format!(
                "local batch accounting drift: ok={ok} map_len={}",
                results_map.len()
            )));
        }

        self.stash
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(batch_id.clone(), results_map);

        Ok(batch_id)
    }
```

### Notes

- `LlmError::Internal(String)` — verify this variant exists in `src/llm/mod.rs::LlmError`. If not, add it: `#[error("Internal error: {0}")] Internal(String)`.
- The `if let Ok(mut map) = results_ref.lock()` at line 196 (worker) intentionally tolerates poison (drops the result) — leave it. The poison cascade fix is the *finalisation* path.
- The same poison pattern exists at line 393-396 for `auth_attempts/auth_failures` increment inside the worker; that's tracked by P2.35 (RB-V1.30-7) as a separate sweep.

---

## P1.10 — LocalProvider unbounded HTTP body read — OOM on hostile/buggy server

**Finding:** P1.10 in audit-triage.md / Robustness RB-V1.30-1
**Files:** `src/llm/local.rs:97-100` (Client builder), `src/llm/local.rs:474-487` (`parse_choices_content`), `src/llm/local.rs:490-500` (`body_preview`)
**Why:** `reqwest::blocking::Client` builder sets timeout + redirect only — no body cap. `resp.json::<Value>()` and `resp.text()` buffer the entire response. A panicked / hostile / misconfigured OpenAI-compat server can return a multi-GB body and OOM the daemon. Up to `local_concurrency()` (≤64) workers compound the risk.

### Current code (src/llm/local.rs:88-115)

```rust
    pub fn new(llm_config: LlmConfig) -> Result<Self, LlmError> {
        let _span = tracing::info_span!("local_provider_new").entered();

        let concurrency = local_concurrency();
        let timeout = local_timeout();
        let api_key = std::env::var("CQS_LLM_API_KEY")
            .ok()
            .filter(|s| !s.is_empty());

        let http = Client::builder()
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()?;

        tracing::info!(
            api_base = %llm_config.api_base,
            model = %llm_config.model,
            concurrency,
            timeout_secs = timeout.as_secs(),
            auth = api_key.is_some(),
            "LocalProvider ready"
        );

        Ok(Self {
            http,
            api_base: llm_config.api_base,
            model: llm_config.model,
```

### Current code (src/llm/local.rs:474-500)

```rust
fn parse_choices_content(resp: reqwest::blocking::Response) -> Result<Option<String>, LlmError> {
    let body: serde_json::Value = resp.json()?;
    // ... unchanged ...
}

fn body_preview(resp: reqwest::blocking::Response) -> String {
    let body = resp.text().unwrap_or_default();
    let cut = body
        .char_indices()
        .nth(256)
        .map(|(i, _)| i)
        .unwrap_or(body.len());
    body[..cut].to_string()
}
```

### Replacement (Client builder, lines 97-100)

No change here — keep timeout + redirect. Apply the cap at the read sites instead.

### Replacement (parse_choices_content, lines 474-488)

```rust
/// Hard cap on response body size. Summary outputs are typically a few hundred
/// bytes; 4 MiB is ~1000× headroom. Larger bodies are a sign of a misbehaving
/// or hostile endpoint and we'd rather error than OOM the daemon.
///
/// Override via `CQS_LOCAL_LLM_MAX_BODY_BYTES`.
fn local_max_body_bytes() -> usize {
    static MAX: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *MAX.get_or_init(|| {
        std::env::var("CQS_LOCAL_LLM_MAX_BODY_BYTES")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|&n: &usize| n > 0)
            .unwrap_or(4 * 1024 * 1024)
    })
}

fn parse_choices_content(resp: reqwest::blocking::Response) -> Result<Option<String>, LlmError> {
    use std::io::Read;
    let cap = local_max_body_bytes();
    let mut buf = Vec::with_capacity(8 * 1024);
    resp.take(cap as u64 + 1).read_to_end(&mut buf).map_err(|e| {
        LlmError::Http(format!("response body read failed: {e}"))
    })?;
    if buf.len() > cap {
        return Err(LlmError::Http(format!(
            "response body exceeds cap ({} > {})",
            buf.len(),
            cap
        )));
    }
    let body: serde_json::Value = serde_json::from_slice(&buf).map_err(|e| {
        LlmError::Http(format!("response body not valid JSON: {e}"))
    })?;
    let content = body
        .get("choices")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|first| first.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string());
    match content {
        Some(s) if !s.is_empty() => Ok(Some(s)),
        _ => Ok(None),
    }
}
```

### Replacement (body_preview, lines 490-500)

```rust
/// Read up to 2 KiB from an HTTP error response body for log context.
/// Returns the empty string if the body can't be read or is non-UTF-8.
/// Hard-capped to bound log spam and prevent OOM on hostile error bodies.
fn body_preview(resp: reqwest::blocking::Response) -> String {
    use std::io::Read;
    const PREVIEW_CAP: u64 = 2 * 1024;
    let mut buf = Vec::with_capacity(PREVIEW_CAP as usize);
    if resp.take(PREVIEW_CAP).read_to_end(&mut buf).is_err() {
        return String::new();
    }
    let body = String::from_utf8_lossy(&buf);
    let cut = body
        .char_indices()
        .nth(256)
        .map(|(i, _)| i)
        .unwrap_or(body.len());
    body[..cut].to_string()
}
```

### Notes

- `reqwest::blocking::Response` implements `Read`, and the builder's `.take(n)` method on a Read returns a length-capped adapter. The wrapping `take(cap as u64 + 1)` is so we can detect "exceeded cap" by reading 1 byte beyond and bailing.
- Confirm `LlmError::Http(String)` variant signature; if it requires a typed inner (e.g. `LlmError::Http(reqwest::Error)`), introduce a sibling `LlmError::BodyTooLarge { got: usize, cap: usize }` instead.
- Add tests per the audit's TC-ADV-1.30-1 (P2.26): `test_oversized_response_body_capped_at_max` and `test_4xx_with_large_body_does_not_buffer_entire_body` in `src/llm/local.rs::tests`.

---

## P1.11 — Auth token leaked into TraceLayer span URI logging

**Finding:** P1.11 in audit-triage.md / Security
**Files:** `src/serve/mod.rs:195` (`TraceLayer::new_for_http()`), interacts with `src/serve/auth.rs:194-232`
**Why:** `TraceLayer::new_for_http()` records `http.uri` (full path + query string) on every span. The first browser navigation `GET /?token=<43 chars>` lands the token in the span at DEBUG. Anyone with `RUST_LOG=tower_http=debug` or running under journald sees the token persisted.

### Current code (src/serve/mod.rs:189-196)

```rust
        // OB-V1.29-5: TraceLayer emits a span per request plus
        // on-response events with latency + status. Handlers already
        // log entry via `tracing::info!`; this layer closes the loop
        // by logging completion, giving per-endpoint latency in the
        // journal without hand-wrapping every handler body.
        .layer(TraceLayer::new_for_http())
}
```

### Replacement

```rust
        // OB-V1.29-5: TraceLayer emits a span per request plus
        // on-response events with latency + status. Handlers already
        // log entry via `tracing::info!`; this layer closes the loop
        // by logging completion, giving per-endpoint latency in the
        // journal without hand-wrapping every handler body.
        //
        // SEC: customise MakeSpan to record path only, NOT the full URI —
        // the `?token=…` query param lands in span fields otherwise and
        // bleeds the per-launch token into journald / RUST_LOG=debug.
        .layer(
            TraceLayer::new_for_http().make_span_with(|req: &axum::extract::Request| {
                tracing::info_span!(
                    "http_request",
                    method = %req.method(),
                    path = %req.uri().path(),
                )
            }),
        )
}
```

### Notes

- The closure drops `query` and `version` fields; if downstream tooling depends on `http.version`, add `version = ?req.version()` (avoid `%` Display because some HTTP version reps may differ across deps).
- `Request` import: `use axum::extract::Request;` — already imported at top of file (line 25).
- Add a regression test in `src/serve/tests.rs::auth_tests` that asserts `?token=…` does not appear in any captured tracing event.

---

## P1.12 — enforce_host_allowlist passes through missing Host header — DNS-rebinding bypass

**Finding:** P1.12 in audit-triage.md / Security + Platform
**Files:** `src/serve/mod.rs:234-251`
**Why:** Doc-comment justifies missing-Host pass-through as "test ergonomic". HTTP/1.0 doesn't require Host; `nc 127.0.0.1 8080` with `GET /api/chunk/<id> HTTP/1.0\r\n\r\n` reaches the handler with zero allowlist check. Test ergonomics privileged over runtime safety.

### Current code (src/serve/mod.rs:230-251)

```rust
/// A missing `Host:` header passes through — HTTP/1.1 requires one and
/// hyper always provides one on real traffic, but unit tests built via
/// `Request::builder()` without a `.uri()` that includes a host don't
/// get one synthesized, and we'd rather not break that ergonomic.
async fn enforce_host_allowlist(
    State(allowed): State<AllowedHosts>,
    req: Request,
    next: Next,
) -> Result<Response, (StatusCode, &'static str)> {
    match req.headers().get(header::HOST) {
        None => Ok(next.run(req).await),
        Some(value) => {
            let host = value.to_str().unwrap_or("");
            if allowed.contains(host) {
                Ok(next.run(req).await)
            } else {
                tracing::warn!(host = %host, "serve: rejected request with disallowed Host header");
                Err((StatusCode::BAD_REQUEST, "disallowed Host header"))
            }
        }
    }
}
```

### Replacement

```rust
/// Reject requests with no `Host:` header. HTTP/1.1 requires one; HTTP/1.0
/// does not, but a no-Host request bypasses DNS-rebinding protection so
/// we treat it as malformed. Tests must build requests with a Host header
/// (see `src/serve/tests.rs` fixtures).
async fn enforce_host_allowlist(
    State(allowed): State<AllowedHosts>,
    req: Request,
    next: Next,
) -> Result<Response, (StatusCode, &'static str)> {
    match req.headers().get(header::HOST) {
        None => {
            tracing::warn!("serve: rejected request with missing Host header");
            Err((StatusCode::BAD_REQUEST, "missing Host header"))
        }
        Some(value) => {
            let host = value.to_str().unwrap_or("");
            if allowed.contains(host) {
                Ok(next.run(req).await)
            } else {
                tracing::warn!(host = %host, "serve: rejected request with disallowed Host header");
                Err((StatusCode::BAD_REQUEST, "disallowed Host header"))
            }
        }
    }
}
```

### Notes

- Existing tests in `src/serve/tests.rs` that use `Request::builder().uri("/foo")` must add `.header(header::HOST, "127.0.0.1:8080")` (or `localhost`) to the builder. Sweep needed:
  - `grep -n 'Request::builder()' src/serve/tests.rs` and add Host headers to fixtures.
- Add a positive test `test_missing_host_header_rejected` that builds a request without Host and asserts 400.

---

## P1.13 — Auth token printed to stdout — captured by journald for 30-day retention

**Finding:** P1.13 in audit-triage.md / Security
**Files:** `src/serve/mod.rs:111-117`
**Why:** Banner `println!("cqs serve listening on http://{actual}/?token={token}")` writes to stdout. Under systemd `StandardOutput=journal` (default) or container log drivers, the token persists into a 30-day-retention store the user doesn't think about. Token doesn't rotate during a long-lived launch.

### Current code (src/serve/mod.rs:105-128)

```rust
        if !quiet {
            // #1096: when auth is enabled, emit the paste-ready URL
            // (token + bind addr) so a fresh launch is one click away
            // from being usable. The token appears here once and is
            // never logged via tracing — auditors can grep for serve
            // banners separately from the structured log stream.
            match auth.as_ref() {
                Some(token) => {
                    println!(
                        "cqs serve listening on http://{actual}/?token={}",
                        token.as_str()
                    );
                }
                None => {
                    println!("cqs serve listening on http://{actual}");
                    eprintln!(
                        "WARN: --no-auth in use — anyone with network access to {actual} \
                         can read this index"
                    );
                }
            }
            println!("press Ctrl-C to stop");
        }
        tracing::info!(addr = %actual, auth_enabled = auth.is_some(), "cqs serve started");
```

### Replacement

```rust
        if !quiet {
            // #1096: when auth is enabled, emit the paste-ready URL
            // (token + bind addr) so a fresh launch is one click away
            // from being usable. The token appears here once and is
            // never logged via tracing — auditors can grep for serve
            // banners separately from the structured log stream.
            //
            // SEC: route the token-bearing banner to STDERR when stdout
            // is not a TTY. systemd `StandardOutput=journal` and container
            // log drivers persist stdout into a 30-day retention store —
            // stderr is similarly captured but is the conventional place
            // for "informational interactive output" and operators can
            // redirect it (`2>/dev/null`) without losing structured logs.
            // For a stronger guarantee, set `--no-banner` (TODO).
            let stdout_is_tty = std::io::IsTerminal::is_terminal(&std::io::stdout());
            match auth.as_ref() {
                Some(token) => {
                    let line = format!(
                        "cqs serve listening on http://{actual}/?token={}",
                        token.as_str()
                    );
                    if stdout_is_tty {
                        println!("{line}");
                    } else {
                        eprintln!("{line}");
                        eprintln!(
                            "(stdout is not a TTY; token-bearing banner routed to stderr to avoid \
                             persisting into journald/container logs)"
                        );
                    }
                }
                None => {
                    println!("cqs serve listening on http://{actual}");
                    eprintln!(
                        "WARN: --no-auth in use — anyone with network access to {actual} \
                         can read this index"
                    );
                }
            }
            println!("press Ctrl-C to stop");
        }
        tracing::info!(addr = %actual, auth_enabled = auth.is_some(), "cqs serve started");
```

### Notes

- `std::io::IsTerminal` stable since 1.70. MSRV is 1.95, fine.
- Stretch (out of scope here): Jupyter-style `--no-banner` flag that writes the URL to a `0o600` file in `$XDG_RUNTIME_DIR` instead. Tracking-issue material.
- Update CHANGELOG: "`cqs serve` no longer prints the auth-bearing URL to stdout when stdout is non-interactive (systemd / container) — banner routes to stderr instead. Set `--no-auth` if banner suppression is required."

---

## P1.14 — cqs serve has no RequestBodyLimitLayer — authenticated client can OOM via large POST

**Finding:** P1.14 in audit-triage.md / Security
**Files:** `src/serve/mod.rs:154-196` (`build_router`)
**Why:** All routes are `GET`, but axum buffers the request body before dispatching. No `RequestBodyLimitLayer` means an authenticated client can `POST /api/stats` with `Content-Length: 99999999999` and OOM the daemon before the 405.

### Current code (src/serve/mod.rs:158-196)

```rust
    let mut app = Router::new()
        .route("/health", get(handlers::health))
        .route("/api/stats", get(handlers::stats))
        // ... routes ...
        .with_state(state);

    if let Some(token) = auth {
        app = app.layer(from_fn_with_state(token, auth::enforce_auth));
    }

    app
        .layer(from_fn_with_state(allowed_hosts, enforce_host_allowlist))
        .layer(CompressionLayer::new())
        .layer(TraceLayer::new_for_http())
}
```

### Replacement

```rust
    let mut app = Router::new()
        .route("/health", get(handlers::health))
        .route("/api/stats", get(handlers::stats))
        // ... routes unchanged ...
        .with_state(state);

    if let Some(token) = auth {
        app = app.layer(from_fn_with_state(token, auth::enforce_auth));
    }

    app
        .layer(from_fn_with_state(allowed_hosts, enforce_host_allowlist))
        // SEC: cap request bodies. Every route is GET; legitimate clients
        // never send a body. 64 KiB is plenty for query strings and cookies
        // (which travel in headers, not body); axum still rejects bodies
        // larger than this with 413 Payload Too Large before allocating.
        .layer(tower_http::limit::RequestBodyLimitLayer::new(64 * 1024))
        .layer(CompressionLayer::new())
        // ... TraceLayer + MakeSpan customisation per P1.11 ...
}
```

### Notes

- `tower_http::limit::RequestBodyLimitLayer` is already in the dep tree (`tower_http` is imported at the top of `serve/mod.rs`); no Cargo.toml change. Verify with `cargo tree -p tower-http -e features 2>&1 | grep limit` — if not enabled, add `"limit"` to the `tower-http` feature list in `Cargo.toml`.
- Layer order matters: this sits *outside* auth/host-allowlist (so the limit applies even to rejected requests, preventing OOM-then-401 attacks) but *inside* compression so the 413 response is still gzipped.

---

## P1.15 — UMAP coords not invalidated on chunk content change — cluster view serves stale

**Finding:** P1.15 in audit-triage.md / Data Safety
**Files:** `src/store/chunks/async_helpers.rs:339-362` (UPSERT)
**Why:** v22 added `umap_x`/`umap_y` columns; `cqs index --umap` writes them. The chunk UPSERT refreshes embedding/embedding_base/etc. on content_hash mismatch but does NOT touch UMAP coords. Cluster view then displays the chunk at a position computed from the old embedding.

### Current code (src/store/chunks/async_helpers.rs:339-362)

```rust
        qb.push(
            " ON CONFLICT(id) DO UPDATE SET \
             origin=excluded.origin, \
             source_type=excluded.source_type, \
             language=excluded.language, \
             chunk_type=excluded.chunk_type, \
             name=excluded.name, \
             signature=excluded.signature, \
             content=excluded.content, \
             content_hash=excluded.content_hash, \
             doc=excluded.doc, \
             line_start=excluded.line_start, \
             line_end=excluded.line_end, \
             embedding=excluded.embedding, \
             embedding_base=excluded.embedding_base, \
             source_mtime=excluded.source_mtime, \
             updated_at=excluded.updated_at, \
             parent_id=excluded.parent_id, \
             window_idx=excluded.window_idx, \
             parent_type_name=excluded.parent_type_name, \
             parser_version=excluded.parser_version \
             WHERE chunks.content_hash != excluded.content_hash \
                OR chunks.parser_version != excluded.parser_version",
        );
```

### Replacement

Add `umap_x = NULL, umap_y = NULL` so the cluster view's `IS NOT NULL` filter correctly reports "needs reprojection". The CASE clause restricts the NULL-out to the content-changed branch — pure parser_version bumps don't invalidate UMAP because the embedding hasn't changed.

```rust
        qb.push(
            " ON CONFLICT(id) DO UPDATE SET \
             origin=excluded.origin, \
             source_type=excluded.source_type, \
             language=excluded.language, \
             chunk_type=excluded.chunk_type, \
             name=excluded.name, \
             signature=excluded.signature, \
             content=excluded.content, \
             content_hash=excluded.content_hash, \
             doc=excluded.doc, \
             line_start=excluded.line_start, \
             line_end=excluded.line_end, \
             embedding=excluded.embedding, \
             embedding_base=excluded.embedding_base, \
             source_mtime=excluded.source_mtime, \
             updated_at=excluded.updated_at, \
             parent_id=excluded.parent_id, \
             window_idx=excluded.window_idx, \
             parent_type_name=excluded.parent_type_name, \
             parser_version=excluded.parser_version, \
             umap_x=CASE WHEN chunks.content_hash != excluded.content_hash \
                         THEN NULL ELSE chunks.umap_x END, \
             umap_y=CASE WHEN chunks.content_hash != excluded.content_hash \
                         THEN NULL ELSE chunks.umap_y END \
             WHERE chunks.content_hash != excluded.content_hash \
                OR chunks.parser_version != excluded.parser_version",
        );
```

### Notes

- Add a regression test in `src/store/chunks/` (or wherever upsert tests live) that:
  1. Inserts a chunk with `umap_x=Some(1.0), umap_y=Some(2.0)` (write via `update_umap_coords_batch`).
  2. UPSERTs the same chunk with new content (different `content_hash`).
  3. Asserts `umap_x IS NULL AND umap_y IS NULL`.
- Stretch (out of scope): metadata `umap_generation` counter to surface "needs reprojection" warnings in `cqs serve`. Tracking-issue material.

---

## P1.16 — --name-boost CLI: defensive clamp at consumer (CLI parser already fixed)

**Finding:** P1.16 in audit-triage.md / Algorithm
**Files:** `src/cli/args.rs:62` (already uses `parse_unit_f32`), `src/search/scoring/candidate.rs:283-289`
**Why:** **LINE DRIFT:** the audit text claims CLI uses `parse_finite_f32`. Verified at `src/cli/args.rs:62`: `value_parser = parse_unit_f32` (clap-bounded `[0.0, 1.0]` per `src/cli/definitions.rs:137-143`). CLI side is already fixed. The remaining defense-in-depth concern: the consumer in `apply_scoring_pipeline` does not clamp, so if `name_boost` ever flows from a path that bypasses `parse_unit_f32` (programmatic library usage, future config-file path that skips `clamp_config_f32`, deserialization), the multiplication still sign-flips.

### Current code (src/search/scoring/candidate.rs:277-289)

```rust
pub(crate) fn apply_scoring_pipeline(
    embedding_score: f32,
    name: Option<&str>,
    file_part: &str,
    ctx: &ScoringContext<'_>,
) -> Option<f32> {
    let base_score = if let Some(matcher) = ctx.name_matcher {
        let n = name.unwrap_or("");
        let name_score = matcher.score(n);
        (1.0 - ctx.filter.name_boost) * embedding_score + ctx.filter.name_boost * name_score
    } else {
        embedding_score
    };
```

### Replacement

```rust
pub(crate) fn apply_scoring_pipeline(
    embedding_score: f32,
    name: Option<&str>,
    file_part: &str,
    ctx: &ScoringContext<'_>,
) -> Option<f32> {
    // Defense-in-depth: clamp name_boost into [0.0, 1.0] regardless of where
    // it originated. CLI uses parse_unit_f32 (clap-bounded) and config uses
    // clamp_config_f32, but a future programmatic / deserialised path could
    // bypass both, in which case `(1.0 - 5.0) * embedding` would sign-flip
    // search results silently. Cheap insurance.
    let name_boost = ctx.filter.name_boost.clamp(0.0, 1.0);
    let base_score = if let Some(matcher) = ctx.name_matcher {
        let n = name.unwrap_or("");
        let name_score = matcher.score(n);
        (1.0 - name_boost) * embedding_score + name_boost * name_score
    } else {
        embedding_score
    };
```

### Notes

- LINE DRIFT noted: searched for `parse_finite_f32` on `name_boost`, found `parse_unit_f32` at `src/cli/args.rs:62`. Audit text was stale; CLI parser fix already shipped (per audit `AC-V1.29-5`). Only the consumer-side defensive clamp remains.
- Add a test `test_apply_scoring_pipeline_clamps_out_of_range_name_boost` in `src/search/scoring/candidate.rs::tests` that constructs a `ScoringContext` with `name_boost = 5.0` and asserts the embedding signal is not negated.

---

## P1.17 — drain_pending_rebuild dedup drops fresh embeddings during rebuild window

**Finding:** P1.17 in audit-triage.md / Algorithm
**Files:** `src/cli/watch.rs:1077-1105`
**Why:** Non-blocking HNSW rebuild (#1113) snapshots `(id, embedding)` from a read-only Store, while the watch loop captures fresh `(id, embedding)` pairs into `pending.delta`. On swap, dedup is by id — if a chunk was re-embedded mid-rebuild (file edit), its fresh embedding lands in delta but `known` already contains the id with the OLD embedding from the snapshot. The filter drops the fresh embedding; HNSW carries stale vector until next threshold rebuild.

### Current code (src/cli/watch.rs:1077-1105)

```rust
    match outcome {
        Ok(Some(mut new_index)) => {
            // Replay captured delta — but skip ids the rebuild thread already
            // saw via its store snapshot, so we don't double-insert. (hnsw_rs
            // has no dedup; duplicate ids would create twin vectors that bloat
            // the graph until the next threshold cleans them up.)
            let known: std::collections::HashSet<&str> =
                new_index.ids().iter().map(String::as_str).collect();
            let to_replay: Vec<(String, Embedding)> = pending
                .delta
                .into_iter()
                .filter(|(id, _)| !known.contains(id.as_str()))
                .collect();
            drop(known);
            if !to_replay.is_empty() {
                let items: Vec<(String, &[f32])> = to_replay
                    .iter()
                    .map(|(id, emb)| (id.clone(), emb.as_slice()))
                    .collect();
                match new_index.insert_batch(&items) {
                    Ok(n) => {
                        tracing::info!(replayed = n, "Replayed delta into rebuilt HNSW before swap")
                    }
                    Err(e) => tracing::warn!(
                        error = %e,
                        replayed_attempt = items.len(),
                        "Failed to replay delta into rebuilt HNSW; new chunks will surface on next rebuild"
                    ),
                }
            }
```

### Replacement

The dedup must compare by content fidelity, not just by id. Options:
1. **Cheapest, correct:** Replay every delta entry unconditionally; on duplicate id, *replace* the snapshot vector. `hnsw_rs` has no replace, so use a "remove then insert" path.
2. **Cheapest, accept stale:** Always insert delta; tolerate duplicate twin vectors until next rebuild (the audit explicitly flags this as a graph-size regression).
3. **Correct, more invasive:** Capture `content_hash` alongside the snapshot, replay only when the delta entry's hash differs.

Pick option 3 for correctness. `pending.delta` becomes `Vec<(String, Embedding, ContentHash)>`, the rebuild thread returns `Vec<(String, ContentHash)>` alongside the index, and the dedup filter checks the hash.

This is **>30 lines of change** spanning `pending` struct definition, the rebuild thread, and the drain — see Notes for the full API change.

### Notes

- **STRUCTURAL CHANGE — exceeds 30-line replacement budget.** API change required:
  - `PendingRebuild::delta: Vec<(String, Embedding)>` → `Vec<(String, Embedding, [u8; 32])>` (or `Vec<(String, Embedding, blake3::Hash)>`).
  - `spawn_hnsw_rebuild` closure (`src/cli/watch.rs:980-1030`) returns `RebuildOutcome` — extend with `snapshot_hashes: Vec<(String, [u8; 32])>` so the drain can dedup by `(id, hash)`.
  - Capture-site: wherever `pending.delta.push(...)` is called (search `pending.delta.push` and the watch reindex path). Each push must include the chunk's `content_hash`.
  - Drain-site replacement (lines 1077-1105):
    ```rust
    let known: std::collections::HashMap<&str, &[u8; 32]> =
        snapshot_hashes.iter().map(|(id, h)| (id.as_str(), h)).collect();
    let to_replay: Vec<(String, Embedding)> = pending
        .delta
        .into_iter()
        .filter(|(id, _, hash)| {
            // Replay if id is unknown OR if the snapshot's vector was built
            // from an older content_hash — fresh embedding wins.
            known.get(id.as_str()).is_none_or(|sh| sh != &hash)
        })
        .map(|(id, emb, _hash)| (id, emb))
        .collect();
    ```
- Add a regression test `test_rebuild_window_re_embedding_replays_fresh_vector` in `src/cli/watch.rs::tests` that:
  1. Spawns a rebuild with snapshot `[("a", emb_v1, hash_v1)]`.
  2. Mid-rebuild, pushes `("a", emb_v2, hash_v2)` to `pending.delta`.
  3. After drain, asserts the swapped HNSW contains `emb_v2`, not `emb_v1`.
- This finding overlaps with P2.29 (TC-ADV: Non-blocking HNSW rebuild — no panic/dim-drift/store-fail tests). Coordinate the fix with the rebuild test suite.

---

## P1.18 — token_pack break-on-first-oversized — drops smaller items that would fit

**Finding:** P1.18 in audit-triage.md / Algorithm
**Files:** `src/cli/commands/mod.rs:398-417`
**Why:** Greedy knapsack: `if used + tokens > budget && kept_any { break; }`. Once one item fails to fit, every lower-scored item is dropped — even items that fit comfortably. Repro: budget=300, items `[A=250, B=100, C=40]`. After A packs (used=250), B fails (350>300) → break → C silently dropped though `used+40=290 ≤ 300`.

### Current code (src/cli/commands/mod.rs:394-422)

```rust
    // Greedy pack in score order, tracking which indices to keep
    let mut used: usize = 0;
    let mut kept_any = false;
    let mut keep: Vec<bool> = vec![false; items.len()];
    for idx in order {
        let tokens = token_counts[idx] + json_overhead_per_item;
        if used + tokens > budget && kept_any {
            break;
        }
        if !kept_any && tokens > budget {
            // Always include at least one result, but cap at 10x budget to avoid
            // pathological cases (e.g., 50K-token item with 300-token budget).
            // When budget == 0, skip the 10x guard (0 * 10 == 0, which would reject
            // every item) and include the first item unconditionally.
            if budget > 0 && tokens > budget * 10 {
                tracing::debug!(tokens, budget, "First item exceeds 10x budget, skipping");
                continue;
            }
            tracing::debug!(
                tokens,
                budget,
                "First item exceeds token budget, including anyway"
            );
        }
        used += tokens;
        keep[idx] = true;
        kept_any = true;
    }
```

### Replacement

```rust
    // Greedy pack in score order, tracking which indices to keep.
    //
    // Note: when an oversized item appears mid-stream we `continue` rather
    // than `break` so subsequent (smaller, lower-scored) items can still
    // fit into the remaining budget. Score-ordered packing already prefers
    // higher-relevance items; the greedy fall-through is the right rounding
    // when one mid-list item won't fit.
    let mut used: usize = 0;
    let mut kept_any = false;
    let mut keep: Vec<bool> = vec![false; items.len()];
    for idx in order {
        let tokens = token_counts[idx] + json_overhead_per_item;
        if used + tokens > budget && kept_any {
            // Skip this oversized item but keep probing — smaller items
            // later in score order may still fit.
            continue;
        }
        if !kept_any && tokens > budget {
            // Always include at least one result, but cap at 10x budget to avoid
            // pathological cases (e.g., 50K-token item with 300-token budget).
            // When budget == 0, skip the 10x guard (0 * 10 == 0, which would reject
            // every item) and include the first item unconditionally.
            if budget > 0 && tokens > budget * 10 {
                tracing::debug!(tokens, budget, "First item exceeds 10x budget, skipping");
                continue;
            }
            tracing::debug!(
                tokens,
                budget,
                "First item exceeds token budget, including anyway"
            );
        }
        used += tokens;
        keep[idx] = true;
        kept_any = true;
    }
```

### Notes

- Single-token change: `break` → `continue`.
- Add a regression test in `src/cli/commands/mod.rs::tests`:
  ```rust
  #[test]
  fn token_pack_continues_past_oversized_midstream_item() {
      // items in score order: oversized, fits, fits
      let items = vec![/* score=1.0 250-token, score=0.9 100-token, score=0.8 40-token */];
      let token_counts = vec![250, 100, 40];
      let (kept, used) = token_pack(&items, &token_counts, /* budget */ 300, /* json_overhead */ 0, /* score_fn */ ...);
      assert_eq!(kept.len(), 2);  // A + C, B skipped
      assert_eq!(used, 290);
  }
  ```

---

## P1.19 — cqs serve shutdown handles only Ctrl-C — SIGTERM (systemctl) skips graceful drain

**Finding:** P1.19 in audit-triage.md / Platform Behavior
**Files:** `src/serve/mod.rs:253-260`
**Why:** `shutdown_signal()` awaits only `tokio::signal::ctrl_c()`. Under systemd or any supervisor that issues `SIGTERM` (default for `systemctl stop`), axum never sees the signal and gets `SIGKILL`'d. macOS `launchd` also sends SIGTERM by default.

### Current code (src/serve/mod.rs:253-260)

```rust
/// Listen for Ctrl-C to trigger axum's graceful shutdown.
async fn shutdown_signal() {
    if let Err(e) = tokio::signal::ctrl_c().await {
        tracing::warn!(error = %e, "failed to install ctrl-c handler; server will only stop on listener failure");
        std::future::pending::<()>().await;
    }
    tracing::info!("ctrl-c received, beginning graceful shutdown");
}
```

### Replacement

```rust
/// Listen for Ctrl-C, SIGTERM (Unix), or Ctrl-Break/Ctrl-Close (Windows) to
/// trigger axum's graceful shutdown. Without SIGTERM handling, `systemctl stop`
/// and `launchd` shutdowns escalate to SIGKILL with no graceful drain.
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::warn!(error = %e, "failed to install ctrl-c handler");
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to install SIGTERM handler");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => tracing::info!("ctrl-c received, beginning graceful shutdown"),
        _ = terminate => tracing::info!("SIGTERM received, beginning graceful shutdown"),
    }
}
```

### Notes

- `tokio::signal::unix::signal` is available with the `signal` feature on tokio. Verify Cargo.toml has `tokio = { ..., features = [..., "signal"] }` — likely already enabled for ctrl_c.
- Stretch: also handle `SignalKind::interrupt()` and Windows `ctrl_break()`/`ctrl_close()`. Skipping for the easy P1 fix; SIGTERM is the high-impact case.
- Companion finding in `src/cli/watch.rs:132-148` already installs a SIGTERM handler via `libc::signal` for the daemon — this fix brings serve to parity.

---

## P1.20 — Default subscriber drops every info_span — 150 spans invisible at default level

**Finding:** P1.20 in audit-triage.md / Observability OB-V1.30-1
**Files:** `src/main.rs:14-32`
**Why:** Default `EnvFilter` is `"warn,ort=error"`, but every span across `scout`, `gather`, `serve`, `cache`, `slot`, parser, store, embedder is `info_span!` or `debug_span!`. Subscriber drops them all. The heavy investment in span instrumentation is invisible until the user discovers `--verbose`/`RUST_LOG=info`. Daemon under `cqs watch --serve` inherits empty `RUST_LOG` and is doubly hit.

### Current code (src/main.rs:14-32)

```rust
fn main() -> Result<()> {
    // Parse CLI first to check verbose flag
    let cli = cli::Cli::parse();

    // Log to stderr to keep stdout clean for structured output
    // --verbose flag sets debug level, otherwise use RUST_LOG or default to warn
    let filter = if cli.verbose {
        EnvFilter::new("debug")
    } else {
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn,ort=error"))
    };

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();

    cli::run_with(cli)
}
```

### Replacement

```rust
fn main() -> Result<()> {
    // Parse CLI first to check verbose flag
    let cli = cli::Cli::parse();

    // Log to stderr to keep stdout clean for structured output.
    // --verbose flag sets debug level for cqs (everything else stays at info),
    // otherwise honour RUST_LOG, defaulting to "cqs=info,warn" so the
    // ~150 span instrumentation sites in the codebase actually render
    // without third-party noise. (OB-V1.30-1.)
    let filter = if cli.verbose {
        EnvFilter::new("cqs=debug,info")
    } else {
        EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new("cqs=info,warn,ort=error"))
    };

    // FmtSpan::CLOSE emits a synthetic event on span close with elapsed time —
    // turns every `info_span!("foo", ...).entered()` into a "foo" + latency
    // line in the journal automatically. Without it, only events emitted
    // *inside* a span produce log lines; entry/exit pairs disappear.
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_span_events(tracing_subscriber::fmt::format::FmtSpan::CLOSE)
        .with_writer(std::io::stderr)
        .init();

    cli::run_with(cli)
}
```

### Notes

- Verify the codebase's actual crate name in Cargo.toml — `cqs` is what `lib.rs` declares per `MEMORY.md`.
- Side effect: every `cqs <command>` now prints span boundaries to stderr at default level. May surprise existing users who pipe stdout but watch stderr. Mention in CHANGELOG: "default log level raised to `cqs=info,warn` so span instrumentation renders by default; set `RUST_LOG=warn` to silence."
- Companion finding P1.21 (auth failures log nothing) and P2.25 (per-request span disconnected from spawn_blocking) become valuable only after this lands.
- Stretch (out of scope): `--log-format=json` flag wired to `.json()` builder for daemon journals. Tracking-issue.

---

## P1.21 — Auth failures log nothing — no journal trail for 401s

**Finding:** P1.21 in audit-triage.md / Observability OB-V1.30-2
**Files:** `src/serve/auth.rs:194-232` (specifically the `AuthOutcome::Unauthorized` arm at lines 224-230)
**Why:** Auth middleware returns 401 silently. Brute-force scans, expired bookmarks, misconfigured clients leave no journal trail. Asymmetric with `enforce_host_allowlist` (`src/serve/mod.rs:246`) which DOES emit `tracing::warn!` on rejection.

### Current code (src/serve/auth.rs:224-230)

```rust
        AuthOutcome::Unauthorized => {
            // Body intentionally minimal: no debug data, no token-
            // length leak. Tracing happens once per launch (banner)
            // and never per-request — auditors can grep for the
            // count of 401s in access logs without seeing tokens.
            (StatusCode::UNAUTHORIZED, "Unauthorized").into_response()
        }
```

### Replacement

```rust
        AuthOutcome::Unauthorized => {
            // Body intentionally minimal: no debug data, no token-
            // length leak. Tracing emits method + path (NOT query
            // string — that may carry `?token=` candidates) so
            // operators get a journal trail for 401s without
            // logging token material.
            tracing::warn!(
                method = %req.method(),
                path = %req.uri().path(),
                "serve: rejected unauthenticated request",
            );
            (StatusCode::UNAUTHORIZED, "Unauthorized").into_response()
        }
```

### Notes

- Critical: log `path` only, NOT `uri()` — the URI carries `?token=…` for the OkViaQueryParam case but the same shape can appear on Unauthorized (bad token still has the param). `req.uri().path()` strips the query string.
- Companion to P1.11 (TraceLayer also leaks query string). Both must be fixed together; otherwise the OB-V1.30-1 default-level fix (P1.20) makes the leak more visible, not less.
- Add a regression test `test_unauthorized_logs_method_and_path_no_query` in `src/serve/auth.rs::tests` that wraps the test subscriber and asserts `?token=...` is absent from captured events.

---
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
# P2 Part B Fix Prompts (P2.47–P2.92)

## P2.47 — reranker compute_scores unchecked batch_size*stride

**Finding:** P2.47 in audit-triage.md
**Files:** `src/reranker.rs:368-415`
**Why:** Listed as algorithm bug; verifying source shows the negative-dim guard AND the `checked_mul` guard already landed.

### Notes

Audit description claimed: "shape[1] = -1 → wraps to usize::MAX" and "batch_size * stride unchecked." Reading `src/reranker.rs:385-405` shows both guards are already present:

```rust
let stride = if shape.len() == 2 {
    let dim = shape[1];
    if dim < 0 {
        return Err(RerankerError::Inference(format!(
            "Model returned negative output dim {dim} (dynamic axis not bound?)"
        )));
    }
    dim as usize
} else { 1 };
if stride == 0 { ... }
let expected_len = batch_size.checked_mul(stride).ok_or_else(|| {
    RerankerError::Inference(format!(
        "Reranker output too large: batch_size={batch_size} * stride={stride} overflows usize"
    ))
})?;
```

**Action:** No-op — finding is already fixed by AC-V1.29-6 comment block. Verifier should mark P2.47 as resolved without code change. Optionally add a regression test that constructs a fake `(shape=[batch,−1])` path through a mock and asserts the negative-dim error is returned (the panic-on-overflow path is covered by `checked_mul`).

---

## P2.48 — doc_comments select_uncached tertiary tie-break

**Finding:** P2.48 in audit-triage.md
**Files:** `src/llm/doc_comments.rs:222-242`
**Why:** Verify whether the chunk-id tie-break is missing.

### Notes

Reading `src/llm/doc_comments.rs:231-239`:

```rust
uncached.sort_by(|a, b| {
    let a_no_doc = a.doc.as_ref().is_none_or(|d| d.trim().is_empty());
    let b_no_doc = b.doc.as_ref().is_none_or(|d| d.trim().is_empty());
    // no-doc before thin-doc
    b_no_doc
        .cmp(&a_no_doc)
        .then_with(|| b.content.len().cmp(&a.content.len()))
        .then_with(|| a.id.cmp(&b.id))
});
```

The tertiary `a.id.cmp(&b.id)` already exists (annotated AC-V1.29-7). **Action:** No-op — already fixed. Verifier should mark P2.48 resolved.

---

## P2.49 — map_hunks_to_functions HashMap iteration order

**Finding:** P2.49 in audit-triage.md
**Files:** `src/impact/diff.rs:38-106` (map_hunks_to_functions), `src/impact/diff.rs:154-168` (cap)
**Why:** `HashMap<&Path, Vec<…>>` is iterated to produce a Vec — non-deterministic when two files exist; downstream `take(cap)` then drops different functions per run.

### Current code

`src/impact/diff.rs:46-65`:

```rust
    // Group hunks by file
    let mut by_file: HashMap<&Path, Vec<&crate::diff_parse::DiffHunk>> = HashMap::new();
    for hunk in hunks {
        by_file.entry(&hunk.file).or_default().push(hunk);
    }

    // PF-1: Batch-fetch all file chunks in a single query instead of N queries
    let normalized_paths: Vec<String> = by_file
        .keys()
        .map(|f| normalize_slashes(&f.to_string_lossy()))
        .collect();
    let origin_refs: Vec<&str> = normalized_paths.iter().map(|s| s.as_str()).collect();
    let chunks_by_origin = match store.get_chunks_by_origins_batch(&origin_refs) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to batch-fetch chunks for diff hunks");
            return functions;
        }
    };

    for (file, file_hunks) in &by_file {
```

### Replacement

After building `functions` via map (or after returning from `map_hunks_to_functions`), sort deterministically. Easiest: change `by_file` to `BTreeMap` so iteration is by path:

```rust
use std::collections::BTreeMap;
// ...
let mut by_file: BTreeMap<&Path, Vec<&crate::diff_parse::DiffHunk>> = BTreeMap::new();
for hunk in hunks {
    by_file.entry(&hunk.file).or_default().push(hunk);
}
```

And, before returning, sort `functions` for full determinism:

```rust
functions.sort_by(|a, b| {
    a.file.cmp(&b.file)
        .then(a.line_start.cmp(&b.line_start))
        .then(a.name.cmp(&b.name))
});
functions
```

### Notes

The `seen: HashSet<String>` dedup uses `chunk.name`, but only first-seen wins — under HashMap order this is also non-deterministic. The final sort eliminates both effects. Add a regression test seeding 3 files with overlapping function names and asserting `map_hunks_to_functions` is identical across 100 calls.

---

## P2.50 — search_reference threshold/weight ordering

**Finding:** P2.50 in audit-triage.md
**Files:** `src/reference.rs:231-285`
**Why:** Underlying search caps at `limit` against unweighted scores AND filters at unweighted threshold; post-weight retain double-filters. Multi-ref ranking under-samples corpus when weight<1.

### Current code

`src/reference.rs:242-258`:

```rust
    let mut results = ref_idx.store.search_filtered_with_index(
        query_embedding,
        filter,
        limit,
        threshold,
        ref_idx.index.as_deref(),
    )?;
    if apply_weight {
        for r in &mut results {
            r.score *= ref_idx.weight;
        }
        // Re-filter after weight: results that passed raw threshold may fall
        // below after weighting (consistent with name_only path)
        results.retain(|r| r.score >= threshold);
    }
    Ok(results)
```

### Replacement

```rust
    let raw_threshold = if apply_weight && ref_idx.weight > 0.0 {
        threshold / ref_idx.weight
    } else {
        threshold
    };
    let raw_limit = if apply_weight {
        // 2× over-fetch leaves headroom for weighted retain step
        limit.saturating_mul(2).max(limit)
    } else {
        limit
    };
    let mut results = ref_idx.store.search_filtered_with_index(
        query_embedding,
        filter,
        raw_limit,
        raw_threshold,
        ref_idx.index.as_deref(),
    )?;
    if apply_weight {
        for r in &mut results {
            r.score *= ref_idx.weight;
        }
        results.retain(|r| r.score >= threshold);
        results.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then(a.chunk.id.cmp(&b.chunk.id))
        });
        results.truncate(limit);
    }
    Ok(results)
```

Mirror the same shape in `search_reference_by_name` at `src/reference.rs:265-285` — its `retain(|r| r.score * weight >= threshold)` already applies the right boundary, but it doesn't over-fetch from `search_by_name`. Pass a relaxed `limit * 2` to `store.search_by_name`, retain+weight+sort+truncate at the end.

### Notes

`SearchResult.chunk.id` (or whatever the canonical id field is) is the deterministic tertiary key. Confirm field path before applying.

---

## P2.51 — find_type_overlap chunk_info HashMap iteration

**Finding:** P2.51 in audit-triage.md
**Files:** `src/related.rs:131-157`
**Why:** Three sources of HashMap iteration leak into `cqs related` output: (a) `chunk_info` `or_insert` retains first arrival, (b) sort lacks tie-break on equal counts, (c) earlier `type_names` collected from HashSet.

### Current code

`src/related.rs:128-157`:

```rust
    let mut type_counts: HashMap<String, u32> = HashMap::new();
    let mut chunk_info: HashMap<String, (PathBuf, u32)> = HashMap::new();

    for chunks in results.values() {
        for chunk in chunks {
            if chunk.name == target_name {
                continue;
            }
            if !matches!(
                chunk.chunk_type,
                crate::language::ChunkType::Function | crate::language::ChunkType::Method
            ) {
                continue;
            }
            *type_counts.entry(chunk.name.clone()).or_insert(0) += 1;
            chunk_info
                .entry(chunk.name.clone())
                .or_insert((chunk.file.clone(), chunk.line_start));
        }
    }

    tracing::debug!(
        candidates = type_counts.len(),
        "Type overlap candidates found"
    );

    // Sort by overlap count descending
    let mut sorted: Vec<(String, u32)> = type_counts.into_iter().collect();
    sorted.sort_by_key(|e| std::cmp::Reverse(e.1));
    sorted.truncate(limit);
```

### Replacement

```rust
    let mut type_counts: HashMap<String, u32> = HashMap::new();
    let mut chunk_info: HashMap<String, (PathBuf, u32)> = HashMap::new();

    // Iterate `results` in deterministic key order so `or_insert` first-wins
    // is reproducible across runs.
    let mut keys: Vec<&String> = results.keys().collect();
    keys.sort();
    for key in keys {
        let chunks = &results[key];
        for chunk in chunks {
            if chunk.name == target_name {
                continue;
            }
            if !matches!(
                chunk.chunk_type,
                crate::language::ChunkType::Function | crate::language::ChunkType::Method
            ) {
                continue;
            }
            *type_counts.entry(chunk.name.clone()).or_insert(0) += 1;
            // Pick min (file, line) so two identical-named functions across files
            // produce a deterministic representative regardless of insertion order.
            let entry = (chunk.file.clone(), chunk.line_start);
            chunk_info
                .entry(chunk.name.clone())
                .and_modify(|cur| {
                    if entry < *cur {
                        *cur = entry.clone();
                    }
                })
                .or_insert(entry);
        }
    }

    tracing::debug!(
        candidates = type_counts.len(),
        "Type overlap candidates found"
    );

    // Sort by count desc, then name asc for stable tie-break.
    let mut sorted: Vec<(String, u32)> = type_counts.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    sorted.truncate(limit);
```

Also, locate the `type_names` collection earlier (~`src/related.rs:59-65`):

```rust
let mut type_names: Vec<&str> = type_set.iter().copied().collect();
type_names.sort();
type_names.dedup();
```

### Notes

Verify the `type_names` site shape before edit — finding cites lines 59-65 but the actual variable name and source set need to be confirmed via Read.

---

## P2.52 — CAGRA search_with_filter under-fills when included<k

**Finding:** P2.52 in audit-triage.md
**Files:** `src/cagra.rs:520-598`
**Why:** When filter retains fewer than `k` candidates, CAGRA is asked for `k` slots and silently returns under-filled results; when `k > itopk_max` AND `included < k`, CAGRA errors and `search_impl` returns empty without retry at feasible `k`.

### Current code

`src/cagra.rs:540-597`:

```rust
        // Build bitset on host: evaluate predicate for each vector
        let n = self.id_map.len();
        let n_words = n.div_ceil(32);
        let mut bitset = vec![0u32; n_words];
        let mut included = 0usize;
        for (i, id) in self.id_map.iter().enumerate() {
            if filter(id) {
                bitset[i / 32] |= 1u32 << (i % 32);
                included += 1;
            }
        }

        // If everything passes the filter, use unfiltered search (faster)
        if included == n {
            return CagraIndex::search(self, query, k);
        }

        // If nothing passes, no results
        if included == 0 {
            return Vec::new();
        }
        // ...
        self.search_impl(&gpu, query, k, Some(&bitset_device))
```

### Replacement

```rust
        // Cap effective k at the count of vectors that actually pass the
        // filter — asking CAGRA for more slots than feasible silently
        // under-fills (or, when k > itopk_max, errors out and zeroes the
        // result). Both modes hide a "no candidates" answer behind the same
        // empty Vec a real "no matches" would produce.
        let effective_k = k.min(included);
        if effective_k < k {
            tracing::debug!(
                requested = k,
                effective = effective_k,
                included,
                "CAGRA filtered search: capping k at included to avoid under-fill"
            );
        }
        // ...
        self.search_impl(&gpu, query, effective_k, Some(&bitset_device))
```

### Notes

Caller (`Store::search_filtered_with_index`) does not currently propagate a `truncated` flag for under-fill; the audit recommends a follow-on but mark out of scope here. The minimal fix is the `effective_k` cap. Add a regression test that builds a 12-vector index, calls `search_with_filter` with `k=20`, asserts result length == 12 and no error logged.

---

## P2.53 — Hybrid SPLADE alpha=0 unbounded score cliff

**Finding:** P2.53 in audit-triage.md
**Files:** `src/search/query.rs:649-672`
**Why:** `alpha == 0` branch emits `1.0 + s` (in `[1.0, 2.0]`) while dense path emits `[-1, 1]` cosine; any positive sparse signal beats every dense match.

### Current code

`src/search/query.rs:649-672`:

```rust
        let mut fused: Vec<crate::index::IndexResult> = all_ids
            .iter()
            .map(|id| {
                let d = dense_scores.get(id).copied().unwrap_or(0.0);
                let s = sparse_scores.get(id).copied().unwrap_or(0.0);
                let score = if alpha <= 0.0 {
                    // Pure re-rank mode: SPLADE score for chunks it found,
                    // cosine score (demoted) for chunks it didn't.
                    // This preserves cosine ordering for SPLADE-unknown chunks
                    // while letting SPLADE override when it has signal.
                    if s > 0.0 {
                        1.0 + s
                    } else {
                        d
                    }
                } else {
                    alpha * d + (1.0 - alpha) * s
                };
                crate::index::IndexResult {
                    id: id.to_string(),
                    score,
                }
            })
            .collect();
```

### Replacement

```rust
        let mut fused: Vec<crate::index::IndexResult> = all_ids
            .iter()
            .map(|id| {
                let d = dense_scores.get(id).copied().unwrap_or(0.0);
                let s = sparse_scores.get(id).copied().unwrap_or(0.0);
                let score = if alpha <= 0.0 {
                    // Pure re-rank mode: SPLADE-found chunks get a small
                    // additive boost over their dense cosine, so SPLADE
                    // signal nudges ranking without dominating it. The
                    // boost stays within the dense [-1, 1] band — no
                    // magic "1.0 + s" cliff that drowns strong cosine
                    // matches under any positive sparse signal.
                    let boost = s * 0.1;
                    d + boost
                } else {
                    alpha * d + (1.0 - alpha) * s
                };
                crate::index::IndexResult {
                    id: id.to_string(),
                    score,
                }
            })
            .collect();
```

### Notes

Add a regression test: dense pool `[(A, 0.95)]`, sparse pool `[(B, 0.001 normalized)]`, alpha=0 → expect `A` first, not `B@1.001`. Eval drift expected — re-run dev-set R@5 after this change.

---

## P2.54 — apply_scoring_pipeline name_boost sign-flip

**Finding:** P2.54 in audit-triage.md
**Files:** `src/search/scoring/candidate.rs:283-298`
**Why:** Out-of-range `name_boost` (CLI accepts arbitrary finite f32) makes `(1 - nb)` negative; `.max(0.0)` then nukes good matches. Even in-range, raw embedding can be negative, contaminating the blend.

### Current code

`src/search/scoring/candidate.rs:282-298`:

```rust
    let base_score = if let Some(matcher) = ctx.name_matcher {
        let n = name.unwrap_or("");
        let name_score = matcher.score(n);
        (1.0 - ctx.filter.name_boost) * embedding_score + ctx.filter.name_boost * name_score
    } else {
        embedding_score
    };

    if let Some(matcher) = ctx.glob_matcher {
        if !matcher.is_match(file_part) {
            return None;
        }
    }

    let chunk_name = name.unwrap_or("");
    let mut score = base_score.max(0.0) * ctx.note_index.boost(file_part, chunk_name);
```

### Replacement

```rust
    // Clamp inputs to [0, 1] before linear interpolation so the blend is
    // always between two same-range numbers and never sign-flips. This
    // closes the failure mode where an out-of-range `name_boost` produces
    // `(1 - nb) < 0`, multiplies a strong embedding match by a negative
    // weight, and the downstream `.max(0.0)` then deletes it silently.
    let embedding_score = embedding_score.clamp(0.0, 1.0);
    let nb = ctx.filter.name_boost.clamp(0.0, 1.0);
    let base_score = if let Some(matcher) = ctx.name_matcher {
        let n = name.unwrap_or("");
        let name_score = matcher.score(n);
        (1.0 - nb) * embedding_score + nb * name_score
    } else {
        embedding_score
    };

    if let Some(matcher) = ctx.glob_matcher {
        if !matcher.is_match(file_part) {
            return None;
        }
    }

    let chunk_name = name.unwrap_or("");
    let mut score = base_score.max(0.0) * ctx.note_index.boost(file_part, chunk_name);
```

### Notes

P1.16 closes the CLI side (clamp at SearchFilter construction). This finding is the in-function defense-in-depth — keep both fixes. Add a property test: for any `name_boost`, `embedding_score`, `name_score` ∈ `f32::finite()`, the output is in `[0.0, ∞)`.

---

## P2.55 — open_browser uses explorer.exe on Windows

**Finding:** P2.55 in audit-triage.md
**Files:** `src/cli/commands/serve.rs:89-104`
**Why:** `explorer.exe <url>` doesn't navigate URLs reliably and can strip `?token=...` query strings. With auth on by default, this breaks the `--open` flow on Windows.

### Current code

`src/cli/commands/serve.rs:87-104`:

```rust
fn open_browser(url: &str) -> Result<()> {
    #[cfg(target_os = "linux")]
    let cmd = "xdg-open";
    #[cfg(target_os = "macos")]
    let cmd = "open";
    #[cfg(target_os = "windows")]
    let cmd = "explorer.exe";

    std::process::Command::new(cmd)
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("Failed to spawn {cmd} {url}"))?;
    Ok(())
}
```

### Replacement

```rust
fn open_browser(url: &str) -> Result<()> {
    // PB-V1.30: on Windows, `explorer.exe <url>` doesn't reliably navigate
    // and can strip query strings (the `?token=...` we depend on for auth).
    // `cmd /C start "" "<url>"` hands the URL to the user's default browser
    // through the documented Win32 protocol-handler path. The empty `""` is
    // required because `start`'s first quoted arg is the window title.
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .with_context(|| format!("Failed to spawn cmd /C start \"\" {url}"))?;
        return Ok(());
    }

    #[cfg(target_os = "linux")]
    let cmd = "xdg-open";
    #[cfg(target_os = "macos")]
    let cmd = "open";

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        std::process::Command::new(cmd)
            .arg(url)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .with_context(|| format!("Failed to spawn {cmd} {url}"))?;
    }
    Ok(())
}
```

---

## P2.56 — NTFS/FAT32 mtime equality

**Finding:** P2.56 in audit-triage.md
**Files:** `src/cli/watch.rs:551-560`
**Why:** Watch loop uses exact `SystemTime` equality on cached mtime. FAT32 USB mounts have 2s mtime resolution — two saves within 2s collide, second save skipped.

### Current code

`src/cli/watch.rs:551-561` (`prune_last_indexed_mtime`) is *not* the equality site — it's the prune. The actual equality check lives in `should_reindex` callers. Audit cites `:551-560` as a proxy / pointer.

### Notes

Locate the actual mtime equality site via grep for `last_indexed_mtime.get` or `last_indexed_mtime` reads against a saved value. It's likely in `process_file_changes` or `should_reindex`. Once located, replace exact `==` with one of:

1. `<` against bucketed mtime when `is_wsl_drvfs_path(path)` is true:
   ```rust
   let stale = if is_wsl_drvfs_path(path) {
       // 2 s buckets — FAT32 mtime granularity floor
       cached_mtime + Duration::from_secs(2) > current_mtime
   } else {
       cached_mtime == current_mtime
   };
   ```
2. Or: fall back to content-hash equality on suspicious mtime ties (parser already computes content hash).

Verifier should grep for the equality site, apply option (1), add a test that constructs two `SystemTime`s 1 second apart, the WSL path triggers the bucketed comparison, the non-WSL path keeps strict equality. Document the FAT32 caveat in the function header.

---

## P2.57 — enforce_host_allowlist accepts missing Host

**Finding:** P2.57 in audit-triage.md
**Files:** `src/serve/mod.rs:230-251`
**Why:** Missing-Host bypass is a unit-test ergonomic in production middleware. HTTP/1.0 + raw nc clients reach the handler with no allowlist check.

### Current code

`src/serve/mod.rs:234-251`:

```rust
async fn enforce_host_allowlist(
    State(allowed): State<AllowedHosts>,
    req: Request,
    next: Next,
) -> Result<Response, (StatusCode, &'static str)> {
    match req.headers().get(header::HOST) {
        None => Ok(next.run(req).await),
        Some(value) => {
            let host = value.to_str().unwrap_or("");
            if allowed.contains(host) {
                Ok(next.run(req).await)
            } else {
                tracing::warn!(host = %host, "serve: rejected request with disallowed Host header");
                Err((StatusCode::BAD_REQUEST, "disallowed Host header"))
            }
        }
    }
}
```

### Replacement

```rust
async fn enforce_host_allowlist(
    State(allowed): State<AllowedHosts>,
    req: Request,
    next: Next,
) -> Result<Response, (StatusCode, &'static str)> {
    let host = match req.headers().get(header::HOST) {
        Some(v) => v.to_str().unwrap_or(""),
        None => {
            // SEC-V1.30: missing-Host is malformed in production (HTTP/1.1
            // requires Host; hyper synthesizes one on real traffic). Reject
            // 400 instead of passing through — closes the DNS-rebinding
            // bypass for HTTP/1.0 clients and raw nc requests. Tests that
            // need to skip the allowlist now stamp a Host header in the
            // Request::builder().
            tracing::warn!("serve: rejected request missing Host header");
            return Err((StatusCode::BAD_REQUEST, "missing Host header"));
        }
    };
    if allowed.contains(host) {
        Ok(next.run(req).await)
    } else {
        tracing::warn!(host = %host, "serve: rejected request with disallowed Host header");
        Err((StatusCode::BAD_REQUEST, "disallowed Host header"))
    }
}
```

### Notes

Existing `src/serve/tests.rs` fixtures via `Request::builder()` need `.header(HOST, "127.0.0.1:8080")` added. P1.12 covers the same bypass at higher priority — confirm this finding hasn't been swept into that fix already; if so, mark resolved.

---

## P2.58 — --bind 0.0.0.0 host-allowlist breaks LAN

**Finding:** P2.58 in audit-triage.md
**Files:** `src/serve/mod.rs:207-218`
**Why:** Wildcard bind populates allowlist with `{loopback, 0.0.0.0, 0.0.0.0:port}` only. LAN clients sending `Host: 192.168.1.5:8080` get 400, push operators to `--no-auth`.

### Current code

`src/serve/mod.rs:207-218`:

```rust
pub(crate) fn allowed_host_set(bind_addr: &SocketAddr) -> AllowedHosts {
    let port = bind_addr.port();
    let mut set = HashSet::new();
    for host in ["localhost", "127.0.0.1", "[::1]"] {
        set.insert(host.to_string());
        set.insert(format!("{host}:{port}"));
    }
    // SocketAddr::to_string wraps IPv6 in brackets automatically.
    set.insert(bind_addr.to_string());
    set.insert(bind_addr.ip().to_string());
    Arc::new(set)
}
```

### Replacement

```rust
pub(crate) fn allowed_host_set(bind_addr: &SocketAddr) -> AllowedHosts {
    let port = bind_addr.port();
    let mut set = HashSet::new();
    for host in ["localhost", "127.0.0.1", "[::1]"] {
        set.insert(host.to_string());
        set.insert(format!("{host}:{port}"));
    }
    set.insert(bind_addr.to_string());
    set.insert(bind_addr.ip().to_string());

    // SEC-V1.30: when binding to a wildcard, we have no way to know which
    // interface IP a legitimate LAN client will dial. Enumerate all local
    // interfaces and add their IPs (plus `:port`) to the allowlist so
    // teammate browsers on the same VLAN don't get 400'd into `--no-auth`.
    if bind_addr.ip().is_unspecified() {
        if let Ok(addrs) = if_addrs::get_if_addrs() {
            for ifa in addrs {
                let ip = ifa.ip().to_string();
                set.insert(ip.clone());
                set.insert(format!("{ip}:{port}"));
            }
        } else {
            tracing::warn!(
                "wildcard bind: failed to enumerate interfaces; LAN clients may hit \
                 disallowed-Host 400. Use an explicit --bind <ip> if this is a problem."
            );
        }
    }
    Arc::new(set)
}
```

### Notes

Adds `if_addrs` workspace dep — confirm via `Cargo.toml` whether it's already pulled in transitively (`notify` may already use it). If a new dep is unwanted, the alternative is to skip the host-header check entirely when `bind.is_unspecified()` and emit a one-line stderr at startup. State the trade-off in the verifier's PR.

---

## P2.59 — Migration restore_from_backup overwrites live DB while pool open

**Finding:** P2.59 in audit-triage.md
**Files:** `src/store/backup.rs:171-180`, `src/store/migrations.rs:106-128`
**Why:** Atomic-replace over `db_path` while the SQLite pool from `migrate()`'s caller still holds open file descriptors. Pool sees old (unlinked) inode; new processes see restored DB. Two-state divergence in daemon contexts.

### Current code

`src/store/backup.rs:171-180`:

```rust
pub(crate) fn restore_from_backup(db_path: &Path, backup_db: &Path) -> Result<(), StoreError> {
    let _span = tracing::info_span!("restore_from_backup").entered();
    copy_triplet(backup_db, db_path)?;
    tracing::info!(
        db = %db_path.display(),
        backup = %backup_db.display(),
        "Restored DB from backup after migration failure"
    );
    Ok(())
}
```

### Replacement

Change the contract: `restore_from_backup` requires the caller to drop the pool first. Update the caller in `src/store/migrations.rs:106-128` to drop pool before calling.

```rust
/// Restore a DB file (+ WAL/SHM sidecars) from a backup.
///
/// # Safety
/// Caller MUST close every pool open against `db_path` BEFORE calling. SQLite
/// in-process pools hold file descriptors against the old inode that the
/// atomic replace unlinks; queries through those descriptors after restore
/// see the unlinked-old inode while new processes see the backup. Two-state
/// divergence is silent — the WAL/SHM sidecars copied alongside the main DB
/// land on the new inode while the pool's mmap'd sidecars belong to the old.
///
/// Public API note: callers that re-open a pool after restore must reopen
/// fresh; the in-process Store handle held during migration is invalid.
pub(crate) fn restore_from_backup(db_path: &Path, backup_db: &Path) -> Result<(), StoreError> {
    let _span = tracing::info_span!("restore_from_backup").entered();
    copy_triplet(backup_db, db_path)?;
    tracing::info!(
        db = %db_path.display(),
        backup = %backup_db.display(),
        "Restored DB from backup after migration failure"
    );
    Ok(())
}
```

And in `src/store/migrations.rs:106-128`, hoist the pool close before the restore call. Use `pool.close().await` (via the existing `rt.block_on`) on every pool the migration owns.

### Notes

Verifier needs to read `migrations.rs:106-128` to identify the actual pool ownership. The minimal fix is correct documentation + caller-side `pool.close().await`. Add `PRAGMA wal_checkpoint(TRUNCATE)` against the live DB before restore to ensure WAL is drained.

---

## P2.60 — stream_summary_writer bypasses WRITE_LOCK

**Finding:** P2.60 in audit-triage.md
**Files:** `src/store/chunks/crud.rs:504-545`
**Why:** Streamed `INSERT OR IGNORE` from LLM provider threads runs against the SqlitePool directly, no WRITE_LOCK. Concurrent reindex contends for SQLite's exclusive lock; per-row implicit transactions are 1 fsync per row.

### Current code

`src/store/chunks/crud.rs:504-540`:

```rust
    pub fn stream_summary_writer(
        &self,
        model: String,
        purpose: String,
    ) -> crate::llm::provider::OnItemCallback {
        use std::sync::Arc;
        let pool = self.pool.clone();
        let rt = Arc::clone(&self.rt);
        Box::new(move |custom_id: &str, text: &str| {
            let now = chrono::Utc::now().to_rfc3339();
            let pool = pool.clone();
            let model = model.clone();
            let purpose = purpose.clone();
            let custom_id = custom_id.to_string();
            let text = text.to_string();
            let result = rt.block_on(async move {
                sqlx::query(
                    "INSERT OR IGNORE INTO llm_summaries \
                     (content_hash, summary, model, purpose, created_at) \
                     VALUES (?, ?, ?, ?, ?)",
                )
                .bind(&custom_id)
                .bind(&text)
                .bind(&model)
                .bind(&purpose)
                .bind(&now)
                .execute(&pool)
                .await
            });
            if let Err(e) = result { /* ... */ }
        })
    }
```

### Replacement

Move the streamed inserts through a buffered queue drained under `begin_write()`. Spawn a single drain task that reads from a `Mutex<Vec<(custom_id, text)>>` flushed every ~200ms or when 64 entries accumulate.

```rust
    pub fn stream_summary_writer(
        &self,
        model: String,
        purpose: String,
    ) -> crate::llm::provider::OnItemCallback {
        use std::sync::Arc;
        // Buffered queue: streamed callbacks push into this Vec; a drain
        // task flushes under begin_write() so all writes serialize through
        // WRITE_LOCK like every other Store mutation. The mutex is local to
        // this writer instance — concurrent stream_summary_writer calls get
        // their own queues.
        let queue: Arc<std::sync::Mutex<Vec<(String, String)>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let rt = Arc::clone(&self.rt);
        let pool = self.pool.clone();
        let write_lock = self.write_lock(); // assuming a getter for WRITE_LOCK Arc<Mutex<()>>
        let queue_drain = Arc::clone(&queue);
        let model_drain = model.clone();
        let purpose_drain = purpose.clone();

        // Spawn drain thread; flushes at most every 200ms or when 64 items
        // queued. Exits when queue is dropped (Arc strong_count reaches 1).
        rt.spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                let drained: Vec<(String, String)> = {
                    let mut q = queue_drain.lock().unwrap_or_else(|p| p.into_inner());
                    if q.is_empty() {
                        if Arc::strong_count(&queue_drain) == 1 { break; }
                        continue;
                    }
                    std::mem::take(&mut *q)
                };
                let _g = write_lock.lock().await;
                let now = chrono::Utc::now().to_rfc3339();
                let mut tx = match pool.begin().await {
                    Ok(t) => t,
                    Err(e) => {
                        tracing::warn!(error = %e, "stream_summary_writer drain begin failed");
                        continue;
                    }
                };
                for (custom_id, text) in drained {
                    let _ = sqlx::query(
                        "INSERT OR IGNORE INTO llm_summaries \
                         (content_hash, summary, model, purpose, created_at) \
                         VALUES (?, ?, ?, ?, ?)",
                    )
                    .bind(&custom_id)
                    .bind(&text)
                    .bind(&model_drain)
                    .bind(&purpose_drain)
                    .bind(&now)
                    .execute(&mut *tx)
                    .await;
                }
                let _ = tx.commit().await;
            }
        });

        Box::new(move |custom_id: &str, text: &str| {
            let mut q = queue.lock().unwrap_or_else(|p| p.into_inner());
            q.push((custom_id.to_string(), text.to_string()));
        })
    }
```

### Notes

Requires exposing `WRITE_LOCK` accessor on `Store`. If not present, expose via `pub(crate) fn write_lock(&self) -> Arc<...>`. Verifier must check the lock implementation (sync `std::sync::Mutex` vs Tokio `Mutex`) and adjust accordingly. The above is a sketch — actual impl needs to match `begin_write()` signatures.

If a full async drain is too invasive, an interim fix is to acquire WRITE_LOCK inside the callback before each insert (still per-row, but properly serialized). That's a smaller diff.

---

## P2.61 — slot_remove TOCTOU on concurrent promote

**Finding:** P2.61 in audit-triage.md
**Files:** `src/cli/commands/infra/slot.rs:299-350`
**Why:** Read active_slot → list_slots → remove_dir_all is non-atomic; concurrent promote can change active between steps, leaving system pointing at deleted slot.

### Current code

`src/cli/commands/infra/slot.rs:299-350` (excerpted):

```rust
fn slot_remove(project_cqs_dir: &Path, name: &str, force: bool, json: bool) -> Result<()> {
    let _span = tracing::info_span!("slot_remove", name, force).entered();
    validate_slot_name(name)?;
    let dir = slot_dir(project_cqs_dir, name);
    if !dir.exists() {
        let available = list_slots(project_cqs_dir).unwrap_or_default().join(", ");
        anyhow::bail!(/*...*/);
    }
    let active = read_active_slot(project_cqs_dir).unwrap_or_else(|| DEFAULT_SLOT.to_string());
    let mut all = list_slots(project_cqs_dir).unwrap_or_default();
    all.retain(|n| n != name);
    if name == active { /*...*/ }
    fs::remove_dir_all(&dir)?;
    /*...*/
}
```

### Replacement

Wrap the entire read-validate-mutate sequence in an exclusive lock on `.cqs/slots.lock`, mirroring `notes.toml.lock`:

```rust
fn slot_remove(project_cqs_dir: &Path, name: &str, force: bool, json: bool) -> Result<()> {
    let _span = tracing::info_span!("slot_remove", name, force).entered();
    validate_slot_name(name)?;

    // Take an exclusive lock so concurrent slot_promote / slot_create /
    // slot_remove can't race the read-validate-mutate sequence below.
    let _slots_lock = cqs::slot::acquire_slots_lock(project_cqs_dir)?;

    let dir = slot_dir(project_cqs_dir, name);
    // ... rest unchanged
}
```

Add `acquire_slots_lock` helper in `src/slot/mod.rs`:

```rust
/// Acquire an exclusive flock on `.cqs/slots.lock`. Held for the duration of
/// any slot lifecycle operation (create/promote/remove) so concurrent calls
/// across processes serialize. Lock file is created if missing.
pub fn acquire_slots_lock(project_cqs_dir: &Path) -> Result<std::fs::File, SlotError> {
    fs::create_dir_all(project_cqs_dir).map_err(|source| SlotError::Io {
        slot: "slots.lock".to_string(),
        source,
    })?;
    let path = project_cqs_dir.join("slots.lock");
    let f = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|source| SlotError::Io {
            slot: "slots.lock".to_string(),
            source,
        })?;
    f.lock().map_err(|source| SlotError::Io {
        slot: "slots.lock".to_string(),
        source,
    })?;
    Ok(f)
}
```

Apply the same `acquire_slots_lock` at the top of `slot_create` and `slot_promote`.

### Notes

`std::fs::File::lock()` is Rust 1.89+, MSRV 1.95 covers it. Rolls together P2.62 and P2.34 (same TOCTOU class). Add a regression test that spawns two threads, each calls `slot_remove` and `slot_promote` for the same target, asserts no orphaned active_slot pointer.

---

## P2.62 — Slot legacy migration moves live WAL/SHM

**Finding:** P2.62 in audit-triage.md
**Files:** `src/slot/mod.rs:511-624`
**Why:** Migration moves `index.db-wal`/`-shm` without checkpointing first. Cross-device fallback is non-atomic — interrupt mid-copy can leave new index.db without WAL, SQLite then truncates uncommitted pages.

### Current code

`src/slot/mod.rs:511-545` (start of `migrate_legacy_index_to_default_slot`):

```rust
pub fn migrate_legacy_index_to_default_slot(project_cqs_dir: &Path) -> Result<bool, SlotError> {
    let _span = tracing::info_span!(
        "migrate_legacy_index_to_default_slot",
        cqs_dir = %project_cqs_dir.display()
    )
    .entered();
    if !project_cqs_dir.exists() { return Ok(false); }
    let slots_dir = slots_root(project_cqs_dir);
    if slots_dir.exists() { return Ok(false); }
    let legacy_index = project_cqs_dir.join(crate::INDEX_DB_FILENAME);
    if !legacy_index.exists() { return Ok(false); }
    let dest = slot_dir(project_cqs_dir, DEFAULT_SLOT);
    fs::create_dir_all(&dest).map_err(/*...*/)?;
    let migration_files = collect_migration_files(project_cqs_dir);
    let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
    for src in &migration_files { /* move_file */ }
    /* ... */
}
```

### Replacement

Insert a WAL-checkpoint step before the file moves so WAL is drained into main DB:

```rust
pub fn migrate_legacy_index_to_default_slot(project_cqs_dir: &Path) -> Result<bool, SlotError> {
    let _span = tracing::info_span!(/*...*/).entered();
    if !project_cqs_dir.exists() { return Ok(false); }
    let slots_dir = slots_root(project_cqs_dir);
    if slots_dir.exists() { return Ok(false); }
    let legacy_index = project_cqs_dir.join(crate::INDEX_DB_FILENAME);
    if !legacy_index.exists() { return Ok(false); }

    // Drain any uncommitted WAL pages into the main DB before we move files.
    // Without this, the move shuffles index.db, index.db-wal, and index.db-shm
    // separately; a non-atomic copy + remove (the EXDEV cross-device fallback
    // in move_file) can interrupt between index.db and index.db-wal, leaving
    // the new slots/default/index.db without its WAL — SQLite then opens the
    // partial DB and silently truncates the missing pages.
    if let Err(e) = checkpoint_legacy_index(&legacy_index) {
        // Non-fatal: the moves still proceed atomically on same-fs renames.
        // On cross-device, the user accepts the remaining risk — log loudly.
        tracing::warn!(
            error = %e,
            "Failed to checkpoint legacy index.db before migration; cross-device move \
             may lose uncommitted WAL pages"
        );
    }

    let dest = slot_dir(project_cqs_dir, DEFAULT_SLOT);
    fs::create_dir_all(&dest).map_err(/*...*/)?;
    /* unchanged */
}

/// Open the legacy DB and run `PRAGMA wal_checkpoint(TRUNCATE)` so the WAL
/// sidecar is empty before the migration moves files. Closes the connection
/// after the pragma so file handles don't leak into the move loop.
fn checkpoint_legacy_index(legacy_index: &Path) -> Result<(), SlotError> {
    let conn = rusqlite::Connection::open(legacy_index)
        .map_err(|e| SlotError::Migration(format!("open legacy db: {e}")))?;
    conn.pragma_update(None, "wal_checkpoint", "TRUNCATE")
        .map_err(|e| SlotError::Migration(format!("checkpoint: {e}")))?;
    Ok(())
}
```

### Notes

If `rusqlite` isn't a dep here, use `sqlx` via a small `tokio::runtime` block. Verifier should pick whichever matches local conventions. Also see P2.34 (rollback half-state) — same migration, related fix; ideally combined into one PR.

---

## P2.63 — model_fingerprint Unix timestamp fallback

**Finding:** P2.63 in audit-triage.md
**Files:** `src/embedder/mod.rs:435-465`
**Why:** Four error branches use `format!("{}:{}", repo, ts)` where ts changes per restart. Cross-slot copy by content_hash is broken; cache writes accumulate as orphans.

### Current code

`src/embedder/mod.rs:435-465` (excerpted):

```rust
                                Err(e) => {
                                    tracing::warn!(error = %e, "Failed to stream-hash model, using repo+timestamp fallback");
                                    let ts = std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .unwrap_or_default()
                                        .as_secs();
                                    format!("{}:{}", self.model_config.repo, ts)
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to open model for fingerprint, using repo+timestamp fallback");
                    let ts = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    format!("{}:{}", self.model_config.repo, ts)
                }
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "Failed to get model paths for fingerprint, using repo+timestamp fallback");
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            format!("{}:{}", self.model_config.repo, ts)
        }
```

### Replacement

Replace the three timestamp fallbacks with a stable shape derived from repo + file size (when available) + a `:fallback` discriminator. Prefer file size when readable; fall back to repo only if size is unavailable.

```rust
/// Stable fallback fingerprint shape — must NOT include any value that
/// changes across process restarts. Cross-slot embedding cache copy by
/// content_hash relies on the model fingerprint matching across runs, so a
/// per-restart timestamp fragments the cache and orphans every fallback
/// embedding. File size is the lightest stable discriminator we can compute
/// without re-reading the file; if even size is unavailable we still want a
/// stable string so multiple fallback runs collide on the same key.
fn fallback_fingerprint(repo: &str, model_path: Option<&Path>) -> String {
    let size = model_path
        .and_then(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .unwrap_or(0);
    format!("{}:fallback:size={}", repo, size)
}
```

Then at each of the three error sites:

```rust
                                Err(e) => {
                                    tracing::warn!(
                                        error = %e,
                                        "Failed to stream-hash model, using stable fallback fingerprint"
                                    );
                                    fallback_fingerprint(&self.model_config.repo, Some(&model_path))
                                }
```

```rust
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "Failed to open model for fingerprint, using stable fallback fingerprint"
                    );
                    fallback_fingerprint(&self.model_config.repo, Some(&model_path))
                }
```

```rust
        Err(e) => {
            tracing::warn!(
                error = %e,
                "Failed to get model paths for fingerprint, using stable fallback fingerprint"
            );
            fallback_fingerprint(&self.model_config.repo, None)
        }
```

### Notes

Verifier must read the surrounding context to thread the actual `model_path` binding into the first two arms (the path is in scope at the inner Err arm because the outer match opened the file). P1.8 covers the same fingerprint failure as a separate finding — confirm both fixes converge to the same `fallback_fingerprint` helper.

---

## P2.64 — Daemon serializes ALL queries through one Mutex

**Finding:** P2.64 in audit-triage.md
**Files:** `src/cli/watch.rs:1775-1858`
**Why:** `Arc<Mutex<BatchContext>>` wraps the entire dispatch path. Slow query (LLM batch fetch, large gather) blocks every other reader. Deadlock surface with stream_summary_writer.

### Current code

`src/cli/watch.rs:1775`:

```rust
                let ctx = Arc::new(Mutex::new(ctx));
```

`src/cli/watch.rs:1853-1858`:

```rust
                            if let Err(e) = std::thread::Builder::new()
                                .name("cqs-daemon-client".to_string())
                                .spawn(move || {
                                    handle_socket_client(stream, &ctx_clone);
                                    in_flight_clone.fetch_sub(1, Ordering::AcqRel);
                                })
```

### Replacement

Convert the outer `Mutex<BatchContext>` to `RwLock<BatchContext>` — read-heavy paths (search, callers, stats) take `read()`; mutation paths (sweep_idle_sessions, reload notes, set_pending_*) take `write()`.

```rust
                let ctx = Arc::new(std::sync::RwLock::new(ctx));
```

Then inside `handle_socket_client` (and the periodic sweep), pick the right lock kind. The sweep at `:1807-1812` becomes:

```rust
                    if last_idle_sweep.elapsed() >= idle_sweep_interval {
                        if let Ok(mut ctx_guard) = ctx.try_write() {
                            ctx_guard.sweep_idle_sessions();
                        }
                        last_idle_sweep = std::time::Instant::now();
                    }
```

Inside `handle_socket_client`, `ctx.read()` for read-only dispatch, `ctx.write()` for mutators. This requires walking the dispatch table to classify each command.

### Notes

This is a non-trivial refactor — the verifier should treat it as a focused PR, not a sweep. Alternative phase 1: keep `Mutex` but split BatchContext into per-resource mutexes (sessions, notes cache, embedder). The audit names both options; pick based on remaining work pressure. State this trade-off explicitly in the PR description.

If RwLock pivot is chosen, audit `stream_summary_writer` (P2.60) — its callbacks fire from outside the daemon thread, must NOT live inside the RwLock guard.

---

## P2.65 — embedding_cache schema purpose conflation

**Finding:** P2.65 in audit-triage.md
**Files:** `src/cache.rs:159-171`
**Why:** Cache PRIMARY KEY is `(content_hash, model_fingerprint)` — no `purpose` column distinguishing `embedding` vs `embedding_base`. Lookups can return wrong vector after #1040 enrichment overwrites only `embedding`.

### Current code

`src/cache.rs:159-178`:

```rust
            sqlx::query(
                "CREATE TABLE IF NOT EXISTS embedding_cache (
                    content_hash TEXT NOT NULL,
                    model_fingerprint TEXT NOT NULL,
                    embedding BLOB NOT NULL,
                    dim INTEGER NOT NULL,
                    created_at INTEGER NOT NULL,
                    PRIMARY KEY (content_hash, model_fingerprint)
                )",
            )
```

### Replacement

Schema migration: add `purpose` column (default `'embedding'`), include in PK. New rows MUST set purpose; old rows take the default.

```rust
            sqlx::query(
                "CREATE TABLE IF NOT EXISTS embedding_cache (
                    content_hash TEXT NOT NULL,
                    model_fingerprint TEXT NOT NULL,
                    purpose TEXT NOT NULL DEFAULT 'embedding',
                    embedding BLOB NOT NULL,
                    dim INTEGER NOT NULL,
                    created_at INTEGER NOT NULL,
                    PRIMARY KEY (content_hash, model_fingerprint, purpose)
                )",
            )
            .execute(&pool)
            .await?;

            // Idempotent migration for existing caches: ALTER TABLE if the
            // column doesn't exist. SQLite ignores the ADD COLUMN if the
            // table is fresh (the CREATE above already includes purpose).
            sqlx::query(
                "ALTER TABLE embedding_cache ADD COLUMN purpose TEXT NOT NULL DEFAULT 'embedding'"
            )
            .execute(&pool)
            .await
            .ok(); // ignore "duplicate column" error on already-migrated caches
```

Then update read/write sites: `read_batch`, `write_batch`, `evict()` queries — every site that touches the cache must bind `purpose`. Find all sites with `grep -n 'embedding_cache' src/cache.rs`.

### Notes

This is a cache schema migration — bumps embedding_cache schema version (separate from main `chunks` schema v22). Document in CHANGELOG. Old cache rows ALTER-defaulted to `'embedding'` is correct because `embedding_base` cache writes have never happened (the audit confirms PR #1040 only writes `embedding`). After this lands, writers that want to cache `embedding_base` pass `purpose='embedding_base'` and lookups disambiguate.

---

## P2.66 — Cache evict() vs write_batch() race

**Finding:** P2.66 in audit-triage.md
**Files:** `src/cache.rs:354-460`
**Why:** `evict()` holds `evict_lock` mutex; `write_batch()` does NOT. Under WAL, evict's BEGIN takes a snapshot; concurrent commit between SELECT-size and DELETE deletes just-inserted rows.

### Current code

`src/cache.rs:354-398` (`write_batch` opens a transaction without `evict_lock`):

```rust
    pub fn write_batch(
        &self,
        entries: &[(&str, &[f32])],
        model_fingerprint: &str,
        dim: usize,
    ) -> Result<usize, CacheError> {
        // ... no evict_lock acquisition ...
        self.rt.block_on(async {
            let mut tx = self.pool.begin().await?;
            // ...
        })
    }
```

`src/cache.rs:408-416` (`evict` acquires `evict_lock`):

```rust
    pub fn evict(&self) -> Result<usize, CacheError> {
        let _span = tracing::info_span!("cache_evict").entered();
        let _guard = self
            .evict_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.rt.block_on(async { /* ... */ })
    }
```

### Replacement

Acquire `evict_lock` in `write_batch` too (same pattern):

```rust
    pub fn write_batch(
        &self,
        entries: &[(&str, &[f32])],
        model_fingerprint: &str,
        dim: usize,
    ) -> Result<usize, CacheError> {
        // DS-V1.30: hold evict_lock across writes too so concurrent evict()
        // can't measure size, then delete rows committed by an in-flight
        // write_batch between its SELECT and DELETE. Without this, a writer
        // sees its INSERT succeed while a cross-session read sees a cache
        // miss — silently re-embedding chunks the cache "should" have.
        let _evict_guard = self
            .evict_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let _span = tracing::info_span!(/* existing */).entered();
        // ... rest unchanged
    }
```

### Notes

Per-batch lock is cheap. Verify naming (`evict_lock` field) by reading `EmbeddingCache` struct definition. If the lock name differs (e.g. `write_lock`), align. Add a regression test that spawns concurrent `write_batch` + `evict` and asserts no row written by `write_batch` is deleted by the racing `evict`.

---

## P2.67 — reindex_files double-parses calls per chunk

**Finding:** P2.67 in audit-triage.md
**Files:** `src/cli/watch.rs:2815, 2930-2939`
**Why:** Watch path uses `parse_file_all` (returns file-level `calls`), then re-runs `extract_calls_from_chunk` per chunk. Bulk pipeline already uses `parse_file_all_with_chunk_calls`. ~14k extra tree-sitter parses per repo-wide reindex.

### Current code

`src/cli/watch.rs:2815, 2930-2939`:

```rust
            match parser.parse_file_all(&abs_path) {
                Ok((mut file_chunks, calls, chunk_type_refs)) => {
                    /* ... */
                    if let Err(e) = store.upsert_function_calls(rel_path, &calls) { /* ... */ }
                    file_chunks
                }
                /* ... */
            }
        })
        .collect();

    /* ... */

    // DS-2: Extract call graph from chunks (same loop), then use atomic upsert.
    let mut calls_by_id: HashMap<String, Vec<cqs::parser::CallSite>> = HashMap::new();
    for chunk in &chunks {
        let calls = parser.extract_calls_from_chunk(chunk);
        if !calls.is_empty() {
            calls_by_id
                .entry(chunk.id.clone())
                .or_default()
                .extend(calls);
        }
    }
```

### Replacement

Switch the inner parse to `parse_file_all_with_chunk_calls`. The fourth tuple element is `Vec<(String, CallSite)>` keyed by absolute-path chunk id; rewrite ids using the same prefix-strip the watch path already does for `chunk.id`, then build `calls_by_id` from the returned chunk_calls without re-parsing.

```rust
            match parser.parse_file_all_with_chunk_calls(&abs_path) {
                Ok((mut file_chunks, calls, chunk_type_refs, chunk_calls)) => {
                    /* path rewrite block unchanged */
                    let abs_norm = cqs::normalize_path(&abs_path);
                    let rel_norm = cqs::normalize_path(rel_path);
                    for chunk in &mut file_chunks {
                        chunk.file = rel_path.clone();
                        if let Some(rest) = chunk.id.strip_prefix(abs_norm.as_str()) {
                            chunk.id = format!("{}{}", rel_norm, rest);
                        }
                    }
                    if !chunk_type_refs.is_empty() {
                        all_type_refs.push((rel_path.clone(), chunk_type_refs));
                    }
                    if let Err(e) = store.upsert_function_calls(rel_path, &calls) { /* ... */ }
                    // Stash chunk-level calls keyed by the post-rewrite chunk id.
                    for (abs_chunk_id, call) in chunk_calls {
                        let chunk_id = match abs_chunk_id.strip_prefix(abs_norm.as_str()) {
                            Some(rest) => format!("{}{}", rel_norm, rest),
                            None => abs_chunk_id,
                        };
                        per_file_chunk_calls.push((chunk_id, call));
                    }
                    file_chunks
                }
                /* ... */
            }
```

Replace the per-chunk `extract_calls_from_chunk` loop with a fold over the collected `per_file_chunk_calls`:

```rust
    let mut calls_by_id: HashMap<String, Vec<cqs::parser::CallSite>> = HashMap::new();
    for (chunk_id, call) in per_file_chunk_calls {
        calls_by_id.entry(chunk_id).or_default().push(call);
    }
```

### Notes

`per_file_chunk_calls` needs to be a top-level `Vec<(String, CallSite)>` accumulator outside the `flat_map`, or threaded via `(file_chunks, Vec<(String, CallSite)>)` tuples. Inspect actual loop shape in watch.rs before applying — the `.collect()` at line 2866 may need restructuring.

---

## P2.68 — reindex_files watch path bypasses global EmbeddingCache

**Finding:** P2.68 in audit-triage.md
**Files:** `src/cli/watch.rs:2876-2887` vs `src/cli/pipeline/embedding.rs:39-62`
**Why:** Watch path only checks `store.get_embeddings_by_hashes`; never sees the per-project `EmbeddingCache` from #1105. File saves in watch mode pay GPU cost for every chunk not in current slot's `chunks.embedding`.

### Current code

`src/cli/watch.rs:2876-2887`:

```rust
    // Check content hash cache to skip re-embedding unchanged chunks
    let hashes: Vec<&str> = chunks.iter().map(|c| c.content_hash.as_str()).collect();
    let existing = store.get_embeddings_by_hashes(&hashes)?;

    let mut cached: Vec<(usize, Embedding)> = Vec::new();
    let mut to_embed: Vec<(usize, &cqs::Chunk)> = Vec::new();
    for (i, chunk) in chunks.iter().enumerate() {
        if let Some(emb) = existing.get(&chunk.content_hash) {
            cached.push((i, emb.clone()));
        } else {
            to_embed.push((i, chunk));
        }
    }
```

### Replacement

Plumb `Option<&EmbeddingCache>` through `cmd_watch` → `WatchConfig` → `reindex_files`. Replace the manual two-tier check with `prepare_for_embedding` from the bulk pipeline:

```rust
    use crate::cli::pipeline::embedding::prepare_for_embedding;
    let prep = prepare_for_embedding(
        &chunks,
        store,
        config.global_cache, // Option<&EmbeddingCache>
        embedder.model_fingerprint(),
        embedder.dim(),
    )?;
    let cached = prep.cached;
    let to_embed = prep.to_embed;
```

(Adapt to the actual `prepare_for_embedding` return shape — read `src/cli/pipeline/embedding.rs:39-82` to confirm the API.)

### Notes

This consolidates P2.67, P2.68, P3.41, P3.42, P3.46 — all watch reindex hot path issues. Verifier may bundle as one PR. The `WatchConfig` struct at `src/cli/watch.rs:572` needs a new field for the global cache. Lifetime threading: `EmbeddingCache` is owned by `cmd_watch`, borrowed for the watch loop's lifetime — straightforward.

---

## P2.69 — wrap_value deep-clones via serde round trip

**Finding:** P2.69 in audit-triage.md
**Files:** `src/cli/json_envelope.rs:160-176`
**Why:** `serde_json::to_value(Envelope::ok(&payload))` walks the entire payload tree and rebuilds it. ~30KB allocator churn per gather call at 100 QPS = ~3MB/s pointless allocations.

### Current code

`src/cli/json_envelope.rs:160-176`:

```rust
pub fn wrap_value(payload: &serde_json::Value) -> serde_json::Value {
    serde_json::to_value(Envelope::ok(payload)).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "wrap_value: envelope serialization failed; emitting fallback shape");
        let owned = payload.clone();
        serde_json::json!({
            "data": owned,
            "error": null,
            "version": JSON_OUTPUT_VERSION,
        })
    })
}
```

### Replacement

Build the envelope as a `serde_json::Map` directly. The shallow clone of the outer payload is unavoidable when callers pass `&Value`; a follow-on can make `wrap_value` take `Value` by value to drop even that.

```rust
pub fn wrap_value(payload: &serde_json::Value) -> serde_json::Value {
    // PF-V1.30: build the envelope as a Map directly. Previously we ran the
    // payload through `serde_json::to_value(Envelope::ok(&payload))` which
    // walks the inner tree and rebuilds every Map/Vec — a deep clone
    // disguised as a re-serialization round trip. The hot-path daemon
    // dispatch wraps tens of KB per query at hundreds of QPS, so the
    // deep clone is real allocator pressure.
    let mut env = serde_json::Map::with_capacity(3);
    env.insert("data".to_string(), payload.clone());
    env.insert("error".to_string(), serde_json::Value::Null);
    env.insert(
        "version".to_string(),
        serde_json::Value::Number(JSON_OUTPUT_VERSION.into()),
    );
    serde_json::Value::Object(env)
}
```

### Notes

Even better follow-on: change `wrap_value(payload: serde_json::Value) -> serde_json::Value` so the outer clone disappears entirely. Most callers (`batch/mod.rs::write_json_line`) already produce the value just-in-time. Out of scope here unless verifier wants to bundle.

---

## P2.70 — build_graph correlated subquery for n_callers

**Finding:** P2.70 in audit-triage.md
**Files:** `src/serve/data.rs:234-264`
**Why:** Per-row `(SELECT COUNT(*) FROM function_calls WHERE callee_name = c.name)` is O(N × log M) where N=ABS_MAX_GRAPH_NODES, M=function_calls row count. `LEFT JOIN (... GROUP BY)` is O(M+N).

### Current code

`src/serve/data.rs:234-264`:

```rust
        let mut node_query = "SELECT c.id, c.name, c.chunk_type, c.language, c.origin, \
                    c.line_start, c.line_end, \
                    COALESCE((SELECT COUNT(*) FROM function_calls fc \
                              WHERE fc.callee_name = c.name), 0) AS n_callers_global \
             FROM chunks c \
             WHERE 1=1"
            .to_string();
        let mut binds: Vec<String> = Vec::new();
        if let Some(file) = file_filter {
            let escaped = file.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
            node_query.push_str(" AND c.origin LIKE ? ESCAPE '\\'");
            binds.push(format!("{escaped}%"));
        }
        if let Some(kind) = kind_filter {
            node_query.push_str(" AND c.chunk_type = ?");
            binds.push(kind.to_string());
        }
        node_query.push_str(" ORDER BY n_callers_global DESC, c.id ASC LIMIT ?");
        binds.push(effective_cap.to_string());
```

### Replacement

```rust
        // PF-V1.30: replace per-row correlated subquery with one aggregated
        // subselect joined by name. Previously each scanned row triggered a
        // log-N index probe into function_calls (~75k probes for a 5000-cap
        // graph against a 30k-edge corpus). One GROUP BY pass is O(M+N).
        let mut node_query = "SELECT c.id, c.name, c.chunk_type, c.language, c.origin, \
                    c.line_start, c.line_end, \
                    COALESCE(cc.n, 0) AS n_callers_global \
             FROM chunks c \
             LEFT JOIN (SELECT callee_name, COUNT(*) AS n \
                        FROM function_calls GROUP BY callee_name) cc \
               ON cc.callee_name = c.name \
             WHERE 1=1"
            .to_string();
```

Rest of the function (file_filter, kind_filter, ORDER BY, LIMIT) is unchanged.

### Notes

`build_hierarchy` at `src/serve/data.rs:670-754` has the same shape per the audit — apply the same JOIN there. Add an explain-plan smoke test if practical, otherwise a benchmark assertion on a large fixture.

---

## P2.71–P2.77, P2.92 — Resource Management cluster

**Finding:** P2.71–P2.77 and P2.92 in audit-triage.md
**Files:** Multiple — see individual sub-sections.
**Why:** Eight resource-management findings introduced in v1.30.0. Most are independent fixes; group together because all are easy-to-medium and share the "v1.30.0 introduced bounded-resource leaks" theme.

---

### P2.71 — Background HNSW rebuild thread detached

**File:** `src/cli/watch.rs:965-1042` (`spawn_hnsw_rebuild`)

#### Current code

`src/cli/watch.rs:1031-1042`:

```rust
    if let Err(e) = thread_result {
        tracing::warn!(error = %e, context, "Failed to spawn HNSW rebuild thread");
    }
    PendingRebuild {
        rx,
        delta: Vec::new(),
        started_at,
    }
```

The `JoinHandle` returned by `thread_result` is `Result<JoinHandle, _>` — currently used only for the spawn-error log. Drop sites the `JoinHandle`.

#### Replacement

Hold the `JoinHandle` inside `PendingRebuild`. On daemon shutdown, `join()` it with a bounded timeout.

```rust
struct PendingRebuild {
    rx: std::sync::mpsc::Receiver<RebuildOutcome>,
    delta: Vec<(String, Embedding)>,
    started_at: std::time::Instant,
    handle: Option<std::thread::JoinHandle<()>>,
}
```

```rust
    let handle = match thread_result {
        Ok(h) => Some(h),
        Err(e) => {
            tracing::warn!(error = %e, context, "Failed to spawn HNSW rebuild thread");
            None
        }
    };
    PendingRebuild { rx, delta: Vec::new(), started_at, handle }
```

On the daemon shutdown path (the `loop` exit in `cmd_watch`), join the handle before letting the daemon exit. If the audit confirms a `state.pending_rebuild.take()` happens during normal swap, this just adds shutdown handling.

### Notes

A bounded timeout via spinning on `JoinHandle::is_finished()` plus a final detached-drop would be the least invasive — full join needs cancellation flag plumbed through `build_hnsw_index_owned`. Audit calls out cancellation as the proper fix; mark as follow-on issue.

---

### P2.72 — pending_rebuild.delta unbounded

**File:** `src/cli/watch.rs:611, 2667-2674, 2740-2741`

#### Current code

`src/cli/watch.rs:611, 623-626`:

```rust
struct PendingRebuild {
    rx: std::sync::mpsc::Receiver<RebuildOutcome>,
    delta: Vec<(String, Embedding)>,
    started_at: std::time::Instant,
}
```

The `delta.push((id, emb))` site at lines ~2667-2674 has no cap.

#### Replacement

Add a cap and a saturation flag:

```rust
const MAX_PENDING_REBUILD_DELTA: usize = 5_000;

// at the push site:
if let Some(ref mut pending) = state.pending_rebuild {
    if pending.delta.len() >= MAX_PENDING_REBUILD_DELTA {
        if !pending.delta_saturated {
            tracing::warn!(
                cap = MAX_PENDING_REBUILD_DELTA,
                "pending HNSW rebuild delta saturated; abandoning in-flight rebuild — \
                 next threshold rebuild will pick up changes from SQLite"
            );
            pending.delta_saturated = true;
        }
        // Drop newest events; the next threshold_rebuild reads from SQLite anyway.
    } else {
        pending.delta.push((chunk_id, embedding));
    }
}
```

Add `delta_saturated: bool` to `PendingRebuild`. On swap, if `delta_saturated`, abandon the rebuilt index (set `pending = None`) so we don't ship a stale snapshot.

### Notes

Combine with P2.71 — same struct, same surgery. Verifier should land both in one PR.

---

### P2.73 — LocalProvider stash retains all submitted batch results

**File:** `src/llm/local.rs:74, 304-309, 542-547`

#### Current code

`src/llm/local.rs:304-311`:

```rust
        let results_map = results.into_inner().unwrap_or_default();
        self.stash
            .lock()
            .unwrap()
            .insert(batch_id.clone(), results_map);

        Ok(batch_id)
```

#### Replacement

Cap stash size and clear failed batches.

```rust
        let results_map = results.into_inner().unwrap_or_default();
        let mut stash = self.stash
            .lock()
            .unwrap_or_else(|p| p.into_inner());

        // Cap total stash entries — if we exceed MAX_STASH_BATCHES, evict
        // oldest by insertion order (HashMap doesn't preserve order; switch
        // to `IndexMap` if available, else use a `VecDeque<String>` of
        // insertion order tracked alongside).
        const MAX_STASH_BATCHES: usize = 128;
        while stash.len() >= MAX_STASH_BATCHES {
            // Pick an arbitrary key to evict — production callers fetch in FIFO
            // order, so any non-current key is dead weight.
            if let Some(stale_key) = stash.keys().next().cloned() {
                stash.remove(&stale_key);
                tracing::warn!(
                    batch_id = %stale_key,
                    "LocalProvider stash exceeded cap; evicting oldest entry"
                );
            } else {
                break;
            }
        }
        stash.insert(batch_id.clone(), results_map);
        drop(stash);
        Ok(batch_id)
```

Also: in the auth-fail Err arm at `:286`, explicitly `stash.remove(&batch_id)` before returning Err.

### Notes

The audit recommends an LRU; `MAX_STASH_BATCHES=128` is a plain cap. If `IndexMap` is not in deps, this is acceptable — the assumption is that production callers drain in submit-order so the cap rarely fires. Add a regression test that submits 200 batches without fetching, asserts `stash.len() == 128`.

---

### P2.74 — Daemon never checks fs.inotify.max_user_watches

**File:** `src/cli/watch.rs:1947-1949`

#### Current code

`src/cli/watch.rs:1947-1949`:

```rust
        Box::new(RecommendedWatcher::new(tx, config)?)
    };
    watcher.watch(&root, RecursiveMode::Recursive)?;
```

#### Replacement

Read `/proc/sys/fs/inotify/max_user_watches` at startup, count directories under `root` honoring gitignore, warn if >90% of limit.

```rust
        Box::new(RecommendedWatcher::new(tx, config)?)
    };

    // RM-V1.30: warn when the project tree approaches the inotify watch
    // limit. notify::watch(Recursive) registers a watch per directory; on
    // distros with the old default of 8192 a moderately-deep monorepo
    // exhausts the limit and per-subdir registration failures are silent.
    #[cfg(target_os = "linux")]
    if !use_poll {
        if let Ok(limit_str) = std::fs::read_to_string("/proc/sys/fs/inotify/max_user_watches") {
            if let Ok(limit) = limit_str.trim().parse::<usize>() {
                let dir_count = count_watchable_dirs(&root);
                if dir_count * 10 > limit * 9 {
                    tracing::warn!(
                        dir_count,
                        limit,
                        "inotify watch limit nearly exhausted; consider \
                         `cqs watch --poll` or `sudo sysctl -w fs.inotify.max_user_watches={}`",
                        limit * 4
                    );
                }
            }
        }
    }

    watcher.watch(&root, RecursiveMode::Recursive)?;
```

```rust
#[cfg(target_os = "linux")]
fn count_watchable_dirs(root: &Path) -> usize {
    let mut count = 0usize;
    let walker = ignore::WalkBuilder::new(root)
        .hidden(false)
        .build();
    for entry in walker.flatten() {
        if entry.file_type().is_some_and(|t| t.is_dir()) {
            count += 1;
        }
    }
    count
}
```

### Notes

`ignore::WalkBuilder` is already a dep (used elsewhere). The alternative — manually descending and registering only non-ignored dirs — is the audit's recommended deeper fix; mark as follow-on issue.

---

### P2.75 — select_provider triggers CUDA probe + symlink ops on every CLI process

**File:** `src/embedder/provider.rs:171-248`, `src/embedder/mod.rs:312-313`

#### Current code

`src/embedder/provider.rs:171-173`:

```rust
pub(crate) fn select_provider() -> ExecutionProvider {
    *CACHED_PROVIDER.get_or_init(detect_provider)
}
```

`Embedder::new` (`src/embedder/mod.rs:312-313`) calls `select_provider()` unconditionally during construction — even on `cqs notes list` / `cqs slot list` / etc. that never run an inference.

#### Replacement

Defer the probe to first inference. Replace eager `select_provider()` call in `Embedder::new` with a lazy `OnceLock<ExecutionProvider>` populated in `Session::create_session`.

The minimal change: introduce `Embedder::provider_lazy()` that calls `select_provider()` on first use, and have `embed_query`/`embed_documents` route through it. `Embedder::new` stops eagerly resolving the provider.

```rust
// In Embedder struct:
provider: std::sync::OnceLock<ExecutionProvider>,

// New helper:
fn provider(&self) -> ExecutionProvider {
    *self.provider.get_or_init(crate::embedder::provider::select_provider)
}

// Session::create_session and other call sites use self.provider() instead
// of self.provider.
```

Update `Embedder::new` to pass the resolved-or-deferred provider to the struct. Remove the eager `select_provider()` call.

### Notes

Verifier needs to read `Embedder::new` and `Session::create_session` signatures to thread this through. The audit's bigger-picture recommendation (move probe inside `Session::create_session`) is the right end state. Pragmatic minimum: keep the `OnceLock` outside session, lazy on first access.

---

### P2.76 — serve handlers spawn_blocking unbounded

**File:** `src/serve/handlers.rs:86-89` + 5 sites + `src/serve/mod.rs:92-95`

#### Current code

`src/serve/mod.rs:92-95`:

```rust
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
```

Default `max_blocking_threads=512`.

#### Replacement

```rust
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(num_cpus::get().min(4))
        .max_blocking_threads(8)
        .enable_all()
        .build()
```

### Notes

8 concurrent SQL queries is plenty for an interactive single-user UI. Combined with worker_threads cap, daemon's max steady-state thread count is bounded at 12 (vs. 512+num_cpus today). Optionally wrap each handler's `spawn_blocking` in `tokio::time::timeout(30s, ...)` — separate change, mark follow-on.

If `num_cpus` not in deps, use `std::thread::available_parallelism()` directly.

---

### P2.77 — Embedder clear_session doubled-memory window

**File:** `src/embedder/mod.rs:261, 808-823`

#### Current code

`src/embedder/mod.rs:808-823`:

```rust
    pub fn clear_session(&self) {
        let mut guard = self.session.lock().unwrap_or_else(|p| p.into_inner());
        *guard = None;
        let mut cache = self.query_cache.lock().unwrap_or_else(|p| p.into_inner());
        cache.clear();
        let mut tok = self.tokenizer.lock().unwrap_or_else(|p| p.into_inner());
        *tok = None;
        tracing::info!("Embedder session, query cache, and tokenizer cleared");
    }
```

#### Replacement

Surface the doubled-memory window via tracing, since the deeper fix (RwLock around tokenizer to wait for in-flight inference) extends the inference critical section.

```rust
    pub fn clear_session(&self) {
        let mut guard = self.session.lock().unwrap_or_else(|p| p.into_inner());
        *guard = None;
        let mut cache = self.query_cache.lock().unwrap_or_else(|p| p.into_inner());
        cache.clear();
        let mut tok = self.tokenizer.lock().unwrap_or_else(|p| p.into_inner());
        // RM-V1.30: surface the doubled-memory window when in-flight
        // inference holds an Arc clone of the tokenizer concurrent with
        // the next-use lazy reload. Strong count > 1 means another thread
        // is mid-encode; the inner Option clears here, but the cloned Arc
        // keeps the old tokenizer alive until that thread releases it,
        // so peak memory transiently exceeds documented ~500MB by the
        // tokenizer size (~10-20MB).
        if let Some(t) = tok.as_ref() {
            let strong = std::sync::Arc::strong_count(t);
            if strong > 1 {
                tracing::info!(
                    strong_count = strong,
                    stage = "clear_during_inference",
                    "tokenizer Arc still referenced by in-flight inference; \
                     transient doubled-memory window during reload"
                );
            }
        }
        *tok = None;
        tracing::info!("Embedder session, query cache, and tokenizer cleared");
    }
```

### Notes

Audit calls option (a) — RwLock around tokenizer with clear taking write lock — as higher-risk because it extends the inference critical section. Option (b) here just surfaces the cost so operators can correlate memory spikes. Mark option (a) as follow-on issue.

---

### P2.92 — Embedder::new opens fresh QueryCache + 7-day prune on every CLI command

**File:** `src/embedder/mod.rs:355-366`

#### Current code

`src/embedder/mod.rs:353-366`:

```rust
        // Best-effort disk cache for query embeddings. Opens a small SQLite
        // DB at ~/.cache/cqs/query_cache.db. Failure is non-fatal.
        let disk_query_cache =
            match crate::cache::QueryCache::open(&crate::cache::QueryCache::default_path()) {
                Ok(c) => {
                    let _ = c.prune_older_than(7);
                    Some(c)
                }
                Err(e) => {
                    tracing::debug!(error = %e, "Disk query cache unavailable (non-fatal)");
                    None
                }
            };
```

#### Replacement

Lazy-open. Replace `Option<QueryCache>` with `OnceLock<Option<QueryCache>>` and open on first `embed_query`.

```rust
// Struct field change:
// disk_query_cache: Option<crate::cache::QueryCache>,
// →
disk_query_cache: std::sync::OnceLock<Option<crate::cache::QueryCache>>,

// In Embedder::new — drop the eager open block. Initialize the OnceLock
// empty:
disk_query_cache: std::sync::OnceLock::new(),

// New accessor:
fn disk_query_cache(&self) -> Option<&crate::cache::QueryCache> {
    self.disk_query_cache
        .get_or_init(|| {
            match crate::cache::QueryCache::open(
                &crate::cache::QueryCache::default_path(),
            ) {
                Ok(c) => {
                    let _ = c.prune_older_than(7);
                    Some(c)
                }
                Err(e) => {
                    tracing::debug!(
                        error = %e,
                        "Disk query cache unavailable (non-fatal)"
                    );
                    None
                }
            }
        })
        .as_ref()
}
```

Update every site that uses `self.disk_query_cache` to call `self.disk_query_cache()`.

### Notes

The audit calls out 16 call sites that construct an embedder via `try_model_config` for commands that never call `embed_query` — `notes list`, `slot list`, `cache stats`. Lazy-open eliminates the WSL DrvFS 30-50ms cold-open per CLI invocation.

---

## P2.78–P2.87 — Test Coverage (happy-path) cluster

**Finding:** P2.78–P2.87 in audit-triage.md
**Why:** Every v1.30.0 surface (#1113 HNSW rebuild, #1114 registry, #1118 auth, #1120 provider, serve data, batch dispatch handlers, LLM passes) shipped without tests. Bundle into a coherent test-debt PR series.

Group structure: each test cluster gets one prompt with a test skeleton. Tests use `InProcessFixture` style seeding.

---

### P2.78 — TC-HAP: serve data endpoints (build_graph, build_chunk_detail, build_hierarchy, build_cluster) untested with populated data

**Files:** `src/serve/data.rs:192,452,586,825,933`, `src/serve/tests.rs:25` (`fixture_state` is empty-only).

#### Test skeleton

Add `src/serve/tests/data_populated.rs` (or extend `tests.rs`):

```rust
// Seed: process_data → validate → format_output, plus one test chunk.
// Assert build_graph returns 3 nodes + 2 call edges; max_nodes=1 truncates;
// kind_filter excludes tests.

#[test]
fn build_graph_returns_seeded_nodes_and_edges() {
    let fx = InProcessFixture::seed_minimal_call_graph();
    let result = build_graph(&fx.store, None, None, None).unwrap();
    assert_eq!(result.nodes.len(), 3);
    assert_eq!(result.edges.len(), 2);
}

#[test]
fn build_graph_max_nodes_truncates() {
    let fx = InProcessFixture::seed_minimal_call_graph();
    let result = build_graph(&fx.store, None, None, Some(1)).unwrap();
    assert_eq!(result.nodes.len(), 1);
}

#[test]
fn build_chunk_detail_returns_callers_callees_tests() {
    let fx = InProcessFixture::seed_minimal_call_graph();
    let detail = build_chunk_detail(&fx.store, "process_data_chunk_id").unwrap().unwrap();
    assert_eq!(detail.callers.len(), 0);
    assert_eq!(detail.callees.len(), 2);
    assert_eq!(detail.tests.len(), 1);
}

#[test]
fn build_hierarchy_callees_returns_subtree() {
    let fx = InProcessFixture::seed_minimal_call_graph();
    let h = build_hierarchy(&fx.store, "process_data", Direction::Callees, 5).unwrap();
    assert_eq!(h.nodes.len(), 3);
}

#[test]
fn build_cluster_returns_nodes_when_umap_populated() {
    let fx = InProcessFixture::seed_with_umap_coords();
    let result = build_cluster(&fx.store, None).unwrap();
    assert!(!result.nodes.is_empty());
}
```

### Notes

`InProcessFixture::seed_minimal_call_graph` doesn't exist yet — needs a small helper that inserts 3 chunks + 2 function_calls rows. Pattern lives in `tests/related_impact_test.rs` or similar; verifier should grep for an existing seeding helper before rolling a new one.

---

### P2.79 — TC-HAP: 16 batch dispatch handlers have zero tests

**Files:** `src/cli/batch/handlers/misc.rs:15,131,173,209` + `graph.rs:24,63,103,143,233,292,375,392` + `info.rs:46,100,168,302`

#### Test skeleton

Add `tests/batch_handlers_test.rs`:

```rust
fn seeded_ctx() -> (BatchContext, Sink) { /* InProcessFixture + tiny corpus */ }

#[test] fn dispatch_callers_round_trips() {
    let (mut ctx, mut sink) = seeded_ctx();
    ctx.dispatch_line("callers process_data", &mut sink).unwrap();
    let env: Value = serde_json::from_slice(&sink.bytes).unwrap();
    assert!(env["data"]["callers"].is_array());
}

// Repeat for: dispatch_callees, dispatch_impact, dispatch_test_map,
// dispatch_trace, dispatch_similar, dispatch_explain, dispatch_context,
// dispatch_deps, dispatch_related, dispatch_impact_diff, dispatch_gather,
// dispatch_scout, dispatch_task, dispatch_where, dispatch_onboard.
```

### Notes

Each test is ~10 lines. Bundle as one file. Use `dispatch_search` test pattern at `src/cli/batch/handlers/search.rs:528-742` as template. Each handler test asserts only envelope shape + a non-empty results array, not algorithmic correctness.

---

### P2.80 — TC-HAP: Reranker rerank/rerank_with_passages no tests

**Files:** `src/reranker.rs:160, 190`

#### Test skeleton

```rust
#[test]
#[ignore] // requires reranker model on disk
fn rerank_preserves_input_set_reorders_by_score() {
    let r = Reranker::new(&Config::default()).unwrap();
    let q = "rust async await";
    let passages = ["tokio runtime docs", "how to bake sourdough", "rust futures trait"];
    let scored: Vec<SearchResult> = passages.iter().enumerate().map(|(i, p)| /*...*/).collect();
    let out = r.rerank(q, scored).unwrap();
    assert_eq!(out.len(), 3, "all 3 passages preserved");
    let last = out.last().unwrap();
    assert!(last.content.contains("sourdough"), "baking ranks last");
}

#[test]
fn rerank_with_passages_empty_input_returns_empty() {
    let r = Reranker::new(&Config::default()).unwrap();
    let out = r.rerank_with_passages("q", vec![], vec![]).unwrap();
    assert!(out.is_empty());
}
```

### Notes

The empty-input test does NOT need the model — it should hit a no-op shortcut. Verify the no-op path exists at the top of `rerank_with_passages`; if not, add it. The model-loading test stays `#[ignore]`-gated.

---

### P2.81 — TC-HAP: cmd_project Search has no CLI integration test

**Files:** `src/cli/commands/infra/project.rs:70` (`cmd_project Search` arm)

#### Test skeleton

Add `tests/cli_project_search_test.rs`:

```rust
#[test]
fn project_search_returns_results_from_each_registered_project() {
    let proj_a = TempProject::with_content(&[("a/foo.rs", "fn process_data() {}")]);
    let proj_b = TempProject::with_content(&[("b/bar.rs", "fn validate() {}")]);
    cqs!(["project", "register", "a", proj_a.root().to_str().unwrap()]);
    cqs!(["project", "register", "b", proj_b.root().to_str().unwrap()]);
    cqs!(["index"], cwd = proj_a.root());
    cqs!(["index"], cwd = proj_b.root());
    let out = cqs!(["project", "search", "process", "--json"]);
    let env: Value = serde_json::from_slice(&out.stdout).unwrap();
    let results = env["data"]["results"].as_array().unwrap();
    let projects: HashSet<&str> = results.iter().map(|r| r["project"].as_str().unwrap()).collect();
    assert!(projects.contains("a"));
    // (project b might or might not match depending on query; relax to "at least one").
    assert!(!results.is_empty());
}
```

### Notes

`tests/cross_project_test.rs` likely has the cross-project fixture; reuse if present. The `cqs!` macro is whatever the project's existing CLI invocation harness uses — grep for usage in `tests/cli_*.rs`.

---

### P2.82 — TC-HAP: cqs ref add/list/remove/update no end-to-end CLI test

**Files:** `src/cli/commands/infra/reference.rs:88, 187, 320, 350`

#### Test skeleton

Add `tests/cli_ref_test.rs`:

```rust
#[test]
fn ref_add_then_list_shows_reference_with_chunk_count() {
    let proj = TempProject::with_content(&[("src/x.rs", "fn foo() {}")]);
    let refp = TempProject::with_content(&[("ref/y.rs", "fn bar() {}"), ("ref/z.rs", "fn baz() {}")]);
    cqs!(["init"], cwd = proj.root());
    cqs!(["index"], cwd = proj.root());
    cqs!(["ref", "add", "lib", refp.root().to_str().unwrap()], cwd = proj.root());
    let out = cqs!(["ref", "list", "--json"], cwd = proj.root());
    let env: Value = serde_json::from_slice(&out.stdout).unwrap();
    let refs = env["data"]["refs"].as_array().unwrap();
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0]["name"], "lib");
    assert!(refs[0]["chunks"].as_u64().unwrap() >= 2);
}

#[test]
fn ref_remove_deletes_from_config_and_disk() { /* ... */ }
#[test]
fn ref_update_reindexes_source_content() { /* ... */ }
#[test]
fn ref_add_weight_rejects_out_of_range() { /* ... */ }
```

### Notes

The `cqs!` invocation pattern + JSON parse is shared across `tests/cli_*.rs`. `weight` must be in `0.0..=1.0` per existing `validate_ref_name` logic.

---

### P2.83 — TC-HAP: handle_socket_client no happy-path round-trip test

**Files:** `src/cli/watch.rs:160`

#### Test skeleton

Add `tests/daemon_socket_roundtrip_test.rs`:

```rust
#[tokio::test(flavor = "multi_thread")]
async fn handle_socket_client_round_trips_stats() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixStream;
    let (mut client, server) = UnixStream::pair().unwrap();
    let server_std = server.into_std().unwrap();
    server_std.set_nonblocking(false).unwrap();

    let store = InProcessFixture::seed_minimal();
    let ctx = BatchContext::new(store);

    // Spawn the server-side handler against the std stream.
    let handle = std::thread::spawn(move || {
        handle_socket_client(server_std, &ctx);
    });

    let request = br#"{"command":"stats","args":[]}\n"#;
    client.write_all(request).await.unwrap();
    let mut buf = Vec::new();
    let _ = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        client.read_to_end(&mut buf),
    ).await;

    let env: Value = serde_json::from_slice(&buf).unwrap();
    assert!(env["data"]["total_chunks"].is_number());
    assert!(env["error"].is_null());
    handle.join().unwrap();
}
```

### Notes

`handle_socket_client` likely takes a `&Mutex<BatchContext>` per current signature — wrap appropriately. `stats` chosen because it needs no embedder. Adjust framing (newline vs length-prefix) by reading the actual `handle_socket_client` impl.

---

### P2.84 — TC-HAP: spawn_hnsw_rebuild/drain_pending_rebuild zero tests

**Files:** `src/cli/watch.rs spawn_hnsw_rebuild` (~965), `drain_pending_rebuild`

#### Test skeleton

Add `src/cli/watch/tests.rs` (or `tests/watch_hnsw_rebuild_test.rs`):

```rust
#[test]
fn rebuild_completes_and_swaps_owned_index() {
    let fx = InProcessFixture::seed_n_chunks(50, dim = 16);
    let pending = spawn_hnsw_rebuild(
        fx.cqs_dir.clone(),
        fx.index_db.clone(),
        16,
        "test",
    );
    let outcome = pending.rx.recv_timeout(Duration::from_secs(30)).unwrap().unwrap();
    let idx = outcome.expect("rebuild produced an index");
    assert_eq!(idx.len(), 50);
}

#[test]
fn delta_replayed_on_swap() { /* seed 50, push 5 deltas mid-rebuild, assert post-swap len == 55 */ }

#[test]
fn delta_dedup_avoids_double_insert() { /* seed 50, push delta with existing id, assert len == 50 */ }
```

### Notes

dim=16 keeps the test fast; `build_hnsw_index_owned` doesn't care about embedding semantics. Verifier needs to spec out the actual `swap` API call sequence — `drain_pending_rebuild` is the consumer in the watch loop.

---

### P2.85 — TC-HAP: for_each_command! macro + 4 emitters no behavioral tests

**Files:** `src/cli/registry.rs:61`, `src/cli/definitions.rs:850,897`, `src/cli/dispatch.rs:51,83`

#### Test skeleton

Add `src/cli/registry.rs::tests`:

```rust
#[test]
fn every_command_variant_has_batch_support_entry() {
    use strum::IntoEnumIterator; // assumes Commands derives EnumIter
    let allowed_none: HashSet<&str> = ["Help", "Version"].iter().copied().collect();
    for v in Commands::iter() {
        let bs = BatchSupport::for_command(&v);
        if matches!(bs, BatchSupport::None) {
            assert!(
                allowed_none.contains(variant_name(&v)),
                "Variant {:?} returns BatchSupport::None but is not on the allowed list",
                variant_name(&v)
            );
        }
    }
}

#[test]
fn group_a_variants_disjoint_from_group_b() {
    let a: HashSet<&str> = group_a_variant_names().into_iter().collect();
    let b: HashSet<&str> = group_b_variant_names().into_iter().collect();
    let inter: Vec<_> = a.intersection(&b).collect();
    assert!(inter.is_empty(), "Variants in both groups: {:?}", inter);
}
```

### Notes

`Commands` may not derive `EnumIter` — if not, hand-roll a `for_each_command!`-driven const list helper. `group_a_variant_names()` / `group_b_variant_names()` need helper functions exposed by the registry. Verifier must wire those up.

`compile_fail` test via `trybuild` was the audit's bonus — out of scope unless `trybuild` is already a dev-dep.

---

### P2.86 — TC-HAP: build_hnsw_index_owned/build_hnsw_base_index no direct tests

**Files:** `src/cli/commands/index/build.rs:848, 880`

#### Test skeleton

Add `src/cli/commands/index/build.rs::tests`:

```rust
#[test]
fn build_hnsw_index_owned_returns_index_with_chunk_count() {
    let fx = InProcessFixture::seed_n_chunks(10, dim = 16);
    let idx = build_hnsw_index_owned(&fx.store, &fx.cqs_dir).unwrap().unwrap();
    assert_eq!(idx.len(), 10);
}

#[test]
fn build_hnsw_base_index_returns_none_when_no_base_rows() {
    let fx = InProcessFixture::empty();
    let result = build_hnsw_base_index(&fx.store, &fx.cqs_dir).unwrap();
    assert!(result.is_none());
}

#[test]
fn build_hnsw_index_owned_round_trips_through_disk() {
    let fx = InProcessFixture::seed_n_chunks(10, dim = 16);
    let idx = build_hnsw_index_owned(&fx.store, &fx.cqs_dir).unwrap().unwrap();
    // Reload from disk:
    let loaded = HnswIndex::load_with_dim(&fx.cqs_dir, "index", 16).unwrap();
    assert_eq!(loaded.len(), idx.len());
    let an_id = idx.ids().iter().next().cloned().unwrap();
    assert!(loaded.ids().contains(&an_id));
}
```

### Notes

`HnswIndex::load_with_dim` API confirm in `src/hnsw/`. dim=16 keeps test fast.

---

### P2.87 — TC-HAP: hyde_query_pass and doc_comment_pass have zero tests

**Files:** `src/llm/hyde.rs:11`, `src/llm/doc_comments.rs:135`

#### Test skeleton

Extend `tests/local_provider_integration.rs`:

```rust
#[test]
fn hyde_query_pass_round_trips_through_mock_server() {
    let fx = InProcessFixture::seed_n_chunks(3, /* with text content */);
    let mock = MockLlmServer::with_canned("hyde response").start();
    std::env::set_var("CQS_LLM_PROVIDER", "local");
    std::env::set_var("CQS_LLM_API_BASE", mock.url());
    let count = hyde_query_pass(&fx.store, /* args */).unwrap();
    assert_eq!(count, 3);
    let rows = fx.store.get_summaries_by_purpose("hyde").unwrap();
    assert_eq!(rows.len(), 3);
}

#[test]
fn doc_comment_pass_skips_already_documented_functions() {
    let fx = InProcessFixture::seed_with_doc_status(&[
        ("foo", false), ("bar", false), ("baz_documented", true),
    ]);
    let mock = MockLlmServer::with_canned("doc response").start();
    std::env::set_var("CQS_LLM_PROVIDER", "local");
    std::env::set_var("CQS_LLM_API_BASE", mock.url());
    let count = doc_comment_pass(&fx.store, /* args */).unwrap();
    assert_eq!(count, 2);
}
```

### Notes

`MockLlmServer` should already exist for the existing `llm_summary_pass` tests in `tests/local_provider_integration.rs:113-280`. Reuse the harness.

---

## P2.88 — Adding third score signal touches two parallel fusion paths

**Finding:** P2.88 in audit-triage.md
**Files:** `src/store/search.rs:182-229`, `src/search/query.rs:511-720`
**Why:** RRF locked to two lists (`semantic_ids`, `fts_ids`); SPLADE fuses on a separate α-blend path. Type boost is a third post-fusion multiplier.

### Notes

This is an extensibility / refactor finding, not a single-line bug. Producing a "minimal change" prompt would understate the scope. Mark as a tracking issue:

- Generalize `Store::rrf_fuse` to `rrf_fuse_n(ranked_lists: &[&[&str]], limit: usize) -> Vec<(String, f32)>`.
- Introduce `trait ScoreSignal { fn rank(&self, query: &Query) -> Vec<&str>; fn weight(&self) -> f32; }` and a `FusionPipeline` that owns an ordered list of signals.
- Migrate semantic + FTS + SPLADE + name-fingerprint + type-boost to uniform participants.

Out of scope for inline fix. **Recommendation:** file as GitHub issue, mark P2.88 as "issue" disposition.

---

## P2.89 — Vector index backend selection is hand-coded if/else

**Finding:** P2.89 in audit-triage.md
**Files:** `src/cli/store.rs:423-540`
**Why:** 120-line `#[cfg(feature = "cuda-index")]` block; new backend = new env var, new branch, new persisted-path literal, new gate. `VectorIndex` trait clean but selector isn't trait-driven.

### Notes

Same shape as P2.88 — extensibility refactor, not a single-line bug. The audit recommends extending `VectorIndex` with `try_open` + `priority` so the selector iterates a `&[&dyn IndexBackend]` slice. Out of scope for inline fix. **Recommendation:** file as issue, mark P2.89 as "issue" disposition.

---

## P2.90 — ScoringOverrides knob → 4 sites; no shared resolver

**Finding:** P2.90 in audit-triage.md
**Files:** `src/config.rs:153-172` + scoring sites
**Why:** Each scoring knob requires editing struct, defaults, env-var resolver, consumer.

### Notes

Same shape — extensibility refactor. Audit recommends `HashMap<&'static str, f32>` + `static SCORING_KNOBS: &[ScoringKnob]` table. Out of scope for inline fix. **Recommendation:** file as issue, mark P2.90 as "issue" disposition.

---

## P2.91 — NoteEntry has no kind/tag taxonomy

**Finding:** P2.91 in audit-triage.md
**Files:** `src/note.rs:41-89`
**Why:** Sentiment-only; no kind field; "TODO" / "design-decision" / "known-bug" must be encoded in note text as unsearchable string patterns.

### Notes

Schema migration + struct change + TOML round-trip + CLI flag — multi-file refactor. **Recommendation:** file as issue, mark P2.91 as "issue" disposition. Inline fix would understate scope.

---
# P3 + P4 fix prompts — v1.30.0 audit

Inputs: `docs/audit-triage.md` + `docs/audit-findings.md`. P3 are minimal Edit-style fix prompts. P4 are paste-ready GitHub issue bodies.

---

## P3.1 — Hoist `panic_message` helper into one place

**Finding:** P3.1
**Files:** `src/cli/pipeline/mod.rs:223-232`, `src/store/mod.rs:1322-1326`, `src/cache.rs:743-747`, `src/cache.rs:1735-1739`
**Why:** Four copies of identical panic-payload extraction logic across 3 modules. Make it `pub(crate)` and use it everywhere.

### Current code
```rust
// src/cli/pipeline/mod.rs:223-232 — pub(crate)? actually `fn` (private)
fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}
```
Plus 3 inline copies inside `Drop` impls (`Store::drop`, `EmbeddingCache::drop`, `QueryCache::drop`) — each a 4-arm `match payload.downcast_ref::<&str>()` ladder.

### Replacement
1. Promote `panic_message` to `pub(crate) fn` in `src/lib.rs` (next to `temp_suffix`) keeping the same signature `&Box<dyn Any + Send> -> String`.
2. Delete the private function in `src/cli/pipeline/mod.rs`; replace the 3 inline copies in the Drop impls with `crate::panic_message(payload)`.

---

## P3.2 — Extract one `find_reference_config` helper for resolve.rs

**Finding:** P3.2
**Files:** `src/cli/commands/resolve.rs:26-39, 46-57`
**Why:** `find_reference` and `resolve_reference_db` re-roll the same `iter().find(|r| r.name == name)` + verbatim error message. Single source of truth.

### Current code
```rust
// resolve.rs:26-39  find_reference
let cfg = config.references.iter()
    .find(|r| r.name == name)
    .ok_or_else(|| anyhow::anyhow!(
        "Reference '{}' not found. Run 'cqs ref list' to see available references.", name
    ))?;
// ...load_references for full ReferenceIndex

// resolve.rs:46-57  resolve_reference_db (inline duplicate)
let cfg = config.references.iter()
    .find(|r| r.name == name)
    .ok_or_else(|| anyhow::anyhow!(
        "Reference '{}' not found. Run 'cqs ref list' to see available references.", name
    ))?;
// uses cfg.path
```

### Replacement
Add a private helper at the top of `resolve.rs`:
```rust
fn find_reference_config<'a>(
    config: &'a Config,
    name: &str,
) -> anyhow::Result<&'a ReferenceConfig> {
    config.references.iter()
        .find(|r| r.name == name)
        .ok_or_else(|| anyhow::anyhow!(
            "Reference '{}' not found. Run 'cqs ref list' to see available references.", name
        ))
}
```
Call it from both sites.

---

## P3.3 + P3.19 — `slot::libc_exdev` hardcode (combined)

**Finding:** P3.3 (cosmetic) + P3.19 (Windows wrong constant)
**Files:** `src/slot/mod.rs:628-647`
**Why:** The `libc_exdev() -> 18` shim is justified by an outdated comment (libc is already a workspace dep), AND it mis-identifies the cross-device error on Windows (`ERROR_NOT_SAME_DEVICE = 17`). Drop the magic number entirely and fall back to copy+remove on any rename failure.

### Current code
```rust
// src/slot/mod.rs:628-647
pub(crate) fn move_file(src: &Path, dst: &Path) -> std::io::Result<()> {
    match fs::rename(src, dst) {
        Ok(()) => Ok(()),
        Err(e) if e.raw_os_error() == Some(libc_exdev()) => {
            fs::copy(src, dst)?;
            fs::remove_file(src)?;
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// EXDEV `errno` value (cross-device link). We hardcode 18 (Linux) since
/// `libc::EXDEV` would pull in a libc dep just for this constant. macOS also
/// uses 18; Windows doesn't surface EXDEV the same way (rename across
/// filesystems just succeeds via the win32 API).
#[inline]
fn libc_exdev() -> i32 { 18 }
```

### Replacement
```rust
pub(crate) fn move_file(src: &Path, dst: &Path) -> std::io::Result<()> {
    match fs::rename(src, dst) {
        Ok(()) => Ok(()),
        // Fall back to copy+remove on ANY rename failure (cross-device,
        // ERROR_NOT_SAME_DEVICE on Windows, EXDEV on Unix). Cheaper than
        // tracking platform-specific errno constants — if the source is
        // gone after the copy, callers see the I/O error from copy() instead.
        Err(_) => {
            fs::copy(src, dst)?;
            fs::remove_file(src)?;
            Ok(())
        }
    }
}
```
Delete `fn libc_exdev()` entirely.

---

## P3.4 — Doc: `enumerate_files` honors .cqsignore too

**Finding:** P3.4
**Files:** `src/lib.rs:542-547`
**Why:** Public-API doc comment claims gitignore-only; body adds `.cqsignore` when `no_ignore=false`.

### Current code
```rust
/// Enumerate files to index in a project directory.
///
/// Respects .gitignore, skips hidden files and files larger than
/// `CQS_MAX_FILE_SIZE` bytes (default 1MB — generated code can exceed this).
/// Returns relative paths from the project root.
///
/// Shared file enumeration for consistent indexing.
pub fn enumerate_files(
```

### Replacement
```rust
/// Enumerate files to index in a project directory.
///
/// Respects `.gitignore` and `.cqsignore` (additive on top of `.gitignore`,
/// both disabled by `no_ignore=true`); skips hidden files and files larger
/// than `CQS_MAX_FILE_SIZE` bytes (default 1 MiB — generated code can
/// exceed this). Returns relative paths from the project root.
pub fn enumerate_files(
```

---

## P3.5 — Drop dead `generate_nl_with_call_context` from public API

**Finding:** P3.5
**Files:** `src/lib.rs:165` (`pub use nl::*`); `src/nl/mod.rs:43-59`
**Why:** Five-arg wrapper that hardcodes `summary=None, hyde=None`. Zero production callers, only test references; leaks via glob re-export.

### Current code
```rust
// src/nl/mod.rs:43-59
pub fn generate_nl_with_call_context(
    chunk: &Chunk, callers: &[String], callees: &[String],
    note: Option<&str>, template: NlTemplate,
) -> String {
    generate_nl_with_call_context_and_summary(
        chunk, callers, callees, note, /*summary=*/ None,
        /*hyde=*/ None, template,
    )
}
```

### Replacement
Delete the wrapper. Update tests in `src/nl/mod.rs` that call it to call `generate_nl_with_call_context_and_summary(.., None, None, ..)` directly. Optionally tighten the glob: replace `pub use nl::*` in `src/lib.rs:165` with an explicit `pub use nl::{NlTemplate, generate_nl_description, generate_nl_with_template, generate_nl_with_call_context_and_summary};`.

---

## P3.6 — Rename `GatherArgs::expand` to `--depth`

**Finding:** P3.6
**Files:** `src/cli/args.rs:GatherArgs::expand`
**Why:** Top-level `--expand-parent` (bool) and `cqs gather --expand <N>` (usize) collide; v1.30.0 only half-fixed the rename. Align `gather` with `onboard`/`impact`/`test-map` which already use `--depth`.

### Current code
```rust
// in GatherArgs (src/cli/args.rs)
#[arg(long, default_value = "2")]
pub expand: usize,
```

### Replacement
```rust
/// Call-graph BFS depth for gather expansion (matches onboard/impact/test-map).
#[arg(long, default_value = "2", visible_alias = "expand")]
pub depth: usize,
```
Sweep references to `args.expand` in `src/cli/commands/search/gather.rs` to `args.depth`.

---

## P3.7 — `cqs eval --save` requires `.json`

**Finding:** P3.7
**Files:** `src/cli/commands/eval/mod.rs` (`EvalCmdArgs::save`); call site that opens the file
**Why:** Accepts any path; eval reports are JSON-only. Asymmetric with `--baseline` which already requires the file exist.

### Current code
```rust
// EvalCmdArgs::save: Option<PathBuf>  — no validation
// In the runner: File::create(&save_path)?
```

### Replacement
At the runner's open site (or top of `cmd_eval`):
```rust
let save_path = args.save.as_deref().map(|p| {
    let ext = p.extension().and_then(|e| e.to_str());
    match ext {
        Some("json") => Ok(p.to_path_buf()),
        Some(other) => anyhow::bail!(
            "--save must end in .json (got .{other}); eval reports are JSON-only"
        ),
        None => {
            let with_ext = p.with_extension("json");
            tracing::info!(path = %with_ext.display(), "appending .json to --save path");
            Ok(with_ext)
        }
    }
}).transpose()?;
```

---

## P3.8 — Eval runner: `eprintln!` → `tracing::info!`

**Finding:** P3.8
**Files:** `src/cli/commands/eval/runner.rs:163-168`
**Why:** Every other progress signal uses `tracing::info!`; `eprintln!` defeats `RUST_LOG` filtering and JSON log redirect.

### Current code
```rust
// runner.rs:167 (approx)
eprintln!("[eval] {done}/{total} queries ({qps:.1} q/s)");
```

### Replacement
```rust
tracing::info!(done, total = total_queries, qps, "eval progress");
```

---

## P3.9 — Add a span to `nl::generate_nl_with_template`

**Finding:** P3.9
**Files:** `src/nl/mod.rs:209` (and transitively covers `:43, :65, :189`)
**Why:** All four NL generators flow into `generate_nl_with_template`; a single `debug_span!` at that root site covers them all.

### Current code
```rust
pub fn generate_nl_with_template(
    chunk: &Chunk, callers: &[String], callees: &[String],
    note: Option<&str>, summary: Option<&str>, hyde: Option<&str>,
    template: NlTemplate,
) -> String {
    // ...
}
```

### Replacement
Insert at line 1 of the body:
```rust
let _span = tracing::debug_span!(
    "generate_nl",
    template = ?template,
    chunk_kind = ?chunk.chunk_type,
    len = chunk.content.len(),
).entered();
```

---

## P3.10 — `embed_documents`/`embed_query` completion events

**Finding:** P3.10
**Files:** `src/embedder/mod.rs:683` (`embed_documents`), `:722` (`embed_query`)
**Why:** Entry spans only carry input fields; no completion event with output dim/count/time.

### Current code
```rust
// inside embed_documents, after the loop returns embeddings:
Ok(embeddings)

// inside embed_query, before returning:
Ok(embedding)
```

### Replacement
At the bottom of `embed_documents` (just before `Ok(embeddings)`):
```rust
tracing::info!(
    total = embeddings.len(),
    dim = self.embedding_dim(),
    input_count = texts.len(),
    "embed_documents complete"
);
```
At the bottom of `embed_query` (just before `Ok(embedding)`):
```rust
tracing::debug!(
    dim = self.embedding_dim(),
    "embed_query complete"
);
```

---

## P3.11 — Reranker `rerank_with_passages` length-mismatch warn + error

**Finding:** P3.11
**Files:** `src/reranker.rs:200-220`
**Why:** When passages.len() != results.len(), the function silently scores arbitrary pairs and corrupts ranks. Hard error + structured warn.

### Current code
```rust
pub fn rerank_with_passages(
    &self, query: &str, passages: &[&str], results: &mut Vec<SearchResult>,
) -> Result<(), RerankerError> {
    let _span = tracing::info_span!("rerank_with_passages",
        n = passages.len()).entered();
    if results.is_empty() { return Ok(()); }
    // ... compute_scores etc.
}
```

### Replacement
After the entry span / early-return:
```rust
if passages.len() != results.len() {
    tracing::warn!(
        passages = passages.len(),
        results = results.len(),
        "rerank_with_passages: length mismatch — caller bug, refusing to score",
    );
    return Err(RerankerError::InvalidArguments(format!(
        "passages.len()={} != results.len()={}",
        passages.len(), results.len()
    )));
}
```
Add the `InvalidArguments(String)` variant to `RerankerError` if not present.

---

## P3.12 — `train_data` git wrappers log non-zero exits

**Finding:** P3.12
**Files:** `src/train_data/git.rs:65-242` (`git_log` ~65, `git_diff_tree` ~131, `git_show` ~173)
**Why:** Each wrapper bundles exit + stderr into an `Err` and returns silently. Operators with shallow clones hit "50% calls fail" with no journal trail.

### Current code (pattern repeated 3x)
```rust
if !output.status.success() {
    return Err(TrainDataError::Git(format!(
        "git diff-tree failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    )));
}
```

### Replacement
At each site, before the early-return:
```rust
if !output.status.success() {
    let stderr = String::from_utf8_lossy(&output.stderr);
    tracing::warn!(
        exit = output.status.code(),
        stderr = %stderr.trim(),
        "git_diff_tree failed",  // change message per fn: git_log / git_show
    );
    return Err(TrainDataError::Git(format!(
        "git diff-tree failed: {}", stderr.trim()
    )));
}
```
Apply consistently to `git_log` (~line 65), `git_diff_tree` (~131), `git_show` (~173). Keep the per-fn message identifier so operators can grep.

---

## P3.13 — Convert format-string `tracing::info!` to structured fields (9 sites)

**Finding:** P3.13
**Files:** `src/hnsw/build.rs:78,236`, `src/hnsw/persist.rs:210,638,771`, `src/reference.rs:220`, `src/cli/commands/train/export_model.rs:76`, `src/audit.rs:85,93`, `src/embedder/provider.rs:149`
**Why:** Format-string interpolation produces unparseable rendered messages once OB-V1.30-1 lands JSON formatting. Pure-mechanical change to structured fields.

### Current code → Replacement (one-pass sweep)
```rust
// src/hnsw/build.rs:78
- tracing::info!("Building HNSW index with {} vectors", nb_elem);
+ tracing::info!(count = nb_elem, "Building HNSW index");

// src/hnsw/build.rs:236
- tracing::info!("HNSW index built: {} vectors", id_map.len());
+ tracing::info!(count = id_map.len(), "HNSW index built");

// src/hnsw/persist.rs:210
- tracing::info!("Saving HNSW index to {}/{}", dir.display(), basename);
+ tracing::info!(dir = %dir.display(), basename, "Saving HNSW index");

// src/hnsw/persist.rs:638
- tracing::info!("Loading HNSW index from {}/{}", dir.display(), basename);
+ tracing::info!(dir = %dir.display(), basename, "Loading HNSW index");

// src/hnsw/persist.rs:771
- tracing::info!("HNSW index loaded: {} vectors", id_map.len());
+ tracing::info!(count = id_map.len(), "HNSW index loaded");

// src/reference.rs:220
- tracing::info!("Loaded {} reference indexes", refs.len());
+ tracing::info!(count = refs.len(), "Loaded reference indexes");

// src/cli/commands/train/export_model.rs:76
- tracing::info!("Model exported to {}", output.display());
+ tracing::info!(output = %output.display(), "Model exported");

// src/audit.rs:85
- tracing::debug!("Failed to parse audit-mode.json: {}", e);
+ tracing::debug!(error = %e, "Failed to parse audit-mode.json");

// src/audit.rs:93
- .map_err(|e| tracing::debug!("Failed to parse expires_at: {}", e))
+ .map_err(|e| tracing::debug!(error = %e, "Failed to parse expires_at"))

// src/embedder/provider.rs:149
- tracing::debug!("Failed to symlink {}: {}", lib, e);
+ tracing::debug!(lib = %lib, error = %e, "Failed to symlink");
```
Verify post-fix with `rg 'tracing::(info|warn|debug|error)!\("[^"]*\{' src/` — should return zero hits in these files.

---

## P3.14 — `build_cluster` warn when corpus has chunks but no UMAP coords

**Finding:** P3.14
**Files:** `src/serve/data.rs:901, 1020` (in `build_cluster`)
**Why:** Empty cluster view leaves operators staring at a blank pane with no journal hint that `cqs index --umap` is needed.

### Current code (sketch — at the end of `build_cluster`)
```rust
Ok(ClusterResponse { nodes, skipped, total_chunks })
```

### Replacement
Right before the return:
```rust
if nodes.is_empty() && skipped > 0 {
    tracing::warn!(
        skipped,
        total_chunks,
        "build_cluster: corpus has chunks but no UMAP coordinates — run `cqs index --umap`",
    );
}
Ok(ClusterResponse { nodes, skipped, total_chunks })
```

---

## P3.15 — Reject leading/trailing-dash slot names

**Finding:** P3.15
**Files:** `src/slot/mod.rs:159-178` (`validate_slot_name`); test block `:661+`
**Why:** `-foo` collides with clap's flag parser; trailing dashes get stripped by various copy-paste pipelines.

### Current code
```rust
pub fn validate_slot_name(name: &str) -> Result<(), SlotError> {
    if name.is_empty() || name.len() > 32 { /* ... */ }
    if !name.chars().all(|c| c.is_ascii_lowercase()
        || c.is_ascii_digit() || c == '_' || c == '-') { /* ... */ }
    Ok(())
}
```

### Replacement
After the existing checks, add:
```rust
if name.starts_with('-') || name.ends_with('-') {
    return Err(SlotError::InvalidName(format!(
        "slot name '{name}' cannot start or end with '-' \
         (clap parses leading dash as a flag)"
    )));
}
```
Add tests in `src/slot/mod.rs::tests`: `validate_rejects_leading_dash`, `validate_rejects_trailing_dash`.

---

## P3.16 — Provider tests for malformed cmdline / `LD_LIBRARY_PATH`

**Finding:** P3.16
**Files:** `src/embedder/provider.rs:67-123`; new `#[cfg(test)] mod tests` in same file
**Why:** No tests in `provider.rs` today; silent CPU fallback on weird inputs is the production failure mode.

### Replacement
Add at the bottom of `src/embedder/provider.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn find_ld_library_dir_skips_empty_entries() {
        let _g = ENV_LOCK.lock().unwrap();
        let prev = env::var_os("LD_LIBRARY_PATH");
        // SAFETY: serialized via ENV_LOCK
        unsafe { env::set_var("LD_LIBRARY_PATH", ":/tmp:"); }
        let dir = find_ld_library_dir();
        // /tmp is the only non-empty entry that exists
        assert_eq!(dir.as_deref(), Some(std::path::Path::new("/tmp")));
        unsafe {
            match prev {
                Some(p) => env::set_var("LD_LIBRARY_PATH", p),
                None => env::remove_var("LD_LIBRARY_PATH"),
            }
        }
    }

    #[test]
    fn find_ld_library_dir_handles_unset() {
        let _g = ENV_LOCK.lock().unwrap();
        let prev = env::var_os("LD_LIBRARY_PATH");
        unsafe { env::remove_var("LD_LIBRARY_PATH"); }
        let dir = find_ld_library_dir();
        assert!(dir.is_none());
        unsafe { if let Some(p) = prev { env::set_var("LD_LIBRARY_PATH", p); } }
    }
}
```

---

## P3.17 — `blake3_hex_or_passthrough` boundary tests

**Finding:** P3.17
**Files:** `src/cache.rs:709-721` + `src/cache.rs::tests`
**Why:** Pin the uppercase / short-hex / passthrough surprises so a future "always-encode" tightening surfaces as an intentional break.

### Replacement
Add to `src/cache.rs::tests`:
```rust
#[test]
fn blake3_hex_or_passthrough_uppercase_64_chars_passthrough() {
    let upper = "ABCDEF0123456789".repeat(4); // 64 chars, all hex
    assert_eq!(blake3_hex_or_passthrough(upper.as_bytes()), upper);
}

#[test]
fn blake3_hex_or_passthrough_short_hex_string_gets_encoded() {
    let short = "abcd"; // 4 hex chars
    let out = blake3_hex_or_passthrough(short.as_bytes());
    assert_eq!(out, "61626364"); // hex of ASCII 'a','b','c','d'
}

#[test]
fn blake3_hex_or_passthrough_64_byte_non_hex_gets_encoded() {
    let bytes = vec![0xAB; 64];
    let out = blake3_hex_or_passthrough(&bytes);
    assert_eq!(out.len(), 128);
    assert!(out.chars().all(|c| c.is_ascii_hexdigit()));
}
```

---

## P3.18 — `SystemTime → i64` cache cast: guard against year-2554 wrap

**Finding:** P3.18
**Files:** `src/cache.rs:349-352, 551-555`
**Why:** `as_secs() as i64` wraps silently above i64::MAX. Defense-in-depth.

### Current code
```rust
// cache.rs:349-352 (write_batch)
let now = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap_or_default()
    .as_secs() as i64;

// cache.rs:551-555 (prune_older_than)
let cutoff = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap_or_default()
    .as_secs() as i64
    - (days as i64 * 86400);
```

### Replacement
Add a helper at module top:
```rust
fn now_unix_i64() -> Result<i64, CacheError> {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| CacheError::Internal("clock before unix epoch".into()))?
        .as_secs();
    i64::try_from(secs)
        .map_err(|_| CacheError::Internal("clock above i64 cap".into()))
}
```
Replace both sites:
```rust
let now = now_unix_i64()?;
// ...
let cutoff = now_unix_i64()? - (days as i64 * 86400);
```

---

## P3.20 — Clamp `cqs cache prune --older-than` to sane ceiling

**Finding:** P3.20
**Files:** `src/cache.rs:548, 551-555`
**Why:** `u32::MAX * 86400` overflow / underflow → silent "prune everything" on typo.

### Current code
```rust
pub fn prune_older_than(&self, days: u32) -> Result<usize, CacheError> {
    let _span = tracing::info_span!("cache_prune", days).entered();
    let cutoff = /* now */ - (days as i64 * 86400);
    // ...
}
```

### Replacement
At the top of the function, clamp + reject:
```rust
pub fn prune_older_than(&self, days: u32) -> Result<usize, CacheError> {
    const MAX_PRUNE_DAYS: u32 = 36_500; // 100 years
    let days = days.min(MAX_PRUNE_DAYS);
    let _span = tracing::info_span!("cache_prune", days).entered();
    let now = now_unix_i64()?; // see P3.18
    let cutoff = now - (days as i64 * 86400);
    if cutoff < 0 {
        return Err(CacheError::Internal(
            format!("prune cutoff below epoch (days={days})")
        ));
    }
    // ...
}
```

---

## P3.21 — Centralize `i64.max(0) as u32` clamp (8+ sites)

**Finding:** P3.21
**Files:** `src/serve/data.rs:290, 299, 300, 587, 588, 777, 778, 993, 994` (verified 9 sites — note line 290 is a `n_callers` clamp, not just line_start/line_end; audit said 8)
**Why:** Repeated open-coded clamp pattern silently masks DB-corruption / migration bugs. Replace with a named helper that logs once on negative input.

### Current code (one example site)
```rust
line_start: line_start.max(0) as u32,
line_end: line_end.max(0) as u32,
```

### Replacement
Add a helper at top of `src/serve/data.rs`:
```rust
/// Clamp an i64 SQL line number to u32, warning once if the input was
/// negative (signals DB corruption or migration bug).
#[inline]
fn clamp_line_to_u32(v: i64) -> u32 {
    if v < 0 {
        tracing::warn!(value = v, "negative line number clamped to 0");
        0
    } else {
        v.min(u32::MAX as i64) as u32
    }
}
```
Sweep all 8/9 occurrences of `<x>.max(0) as u32` to `clamp_line_to_u32(<x>)`. Verify with `rg 'max\(0\) as u32' src/serve/data.rs` returning zero hits.

---

## P3.22 — Daemon socket-thread join: warn on detach-after-timeout

**Finding:** P3.22
**Files:** `src/cli/watch.rs:2374-2400`
**Why:** Doc-comment claims "joined cleanly" but deadline-fall-through silently detaches the thread. Add the warn so logs match reality.

### Current code (sketch — the polling loop)
```rust
let deadline = Instant::now() + Duration::from_secs(5);
loop {
    if handle.is_finished() {
        let _ = handle.join();
        tracing::info!("Daemon socket thread joined cleanly");
        break;
    }
    if Instant::now() > deadline { break; }
    std::thread::sleep(Duration::from_millis(50));
}
```

### Replacement
```rust
let deadline = Instant::now() + Duration::from_secs(5);
let mut joined = false;
loop {
    if handle.is_finished() {
        let _ = handle.join();
        tracing::info!("Daemon socket thread joined cleanly");
        joined = true;
        break;
    }
    if Instant::now() > deadline { break; }
    std::thread::sleep(Duration::from_millis(50));
}
if !joined {
    tracing::warn!(
        "Daemon socket thread did not exit within 5s; detaching"
    );
}
```

---

## P3.23 — `diff::EMBEDDING_BATCH_SIZE` env override

**Finding:** P3.23
**Files:** `src/diff.rs:158`
**Why:** Hardcoded 1000 doesn't scale with model dim; ~12 MB only at 1024-dim.

### Current code
```rust
const EMBEDDING_BATCH_SIZE: usize = 1000;
```

### Replacement
```rust
fn embedding_batch_size() -> usize {
    std::env::var("CQS_DIFF_EMBEDDING_BATCH_SIZE")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(1000)
}
```
Replace the const reference at the call site with `embedding_batch_size()`. Update the surrounding comment to mention `CQS_DIFF_EMBEDDING_BATCH_SIZE` and the dim sensitivity.

---

## P3.24 — Daemon `worker_threads` env override

**Finding:** P3.24
**Files:** `src/cli/watch.rs:115-119`
**Why:** Hardcoded `min(num_cpus, 4)` caps large-machine parallelism with no escape hatch.

### Current code
```rust
let worker_threads = std::thread::available_parallelism()
    .map(|n| n.get()).unwrap_or(1).min(4);
```

### Replacement
```rust
let worker_threads = std::env::var("CQS_DAEMON_WORKER_THREADS")
    .ok().and_then(|v| v.parse::<usize>().ok()).filter(|&n| n > 0)
    .unwrap_or_else(|| {
        std::thread::available_parallelism()
            .map(|n| n.get()).unwrap_or(1).min(4)
    });
```

---

## P3.25 — `train_data::MAX_SHOW_SIZE` env override

**Finding:** P3.25
**Files:** `src/train_data/git.rs:167`; ideally moved into `src/limits.rs`
**Why:** Hardcoded 50 MB silently drops large files from training-data extraction with no log signal.

### Current code
```rust
const MAX_SHOW_SIZE: usize = 50 * 1024 * 1024;
```

### Replacement
```rust
fn max_show_size() -> usize {
    std::env::var("CQS_TRAIN_GIT_SHOW_MAX_BYTES")
        .ok().and_then(|v| v.parse::<usize>().ok()).filter(|&n| n > 0)
        .unwrap_or(50 * 1024 * 1024)
}
```
Update the caller to call `max_show_size()` and add a `tracing::warn!(path = %path, size, max, "git_show output exceeds max — skipping")` at the early-return site so callers can distinguish "too large" from "binary".

---

## P3.26 — Lift `BatchCmd::is_pipeable` into the registry

**Finding:** P3.26
**Files:** `src/cli/batch/commands.rs:325-364` (`is_pipeable`); registry at `src/cli/registry.rs`
**Why:** #1114 collapsed Group-A/B exhaustive matches but the batch-side `is_pipeable` enum-match was missed; one row per command becomes one row + one batch arm.

### Current code
```rust
// src/cli/batch/commands.rs:325-364
impl BatchCmd {
    pub fn is_pipeable(&self) -> bool {
        match self {
            BatchCmd::Search { .. } | BatchCmd::Gather { .. }
            | BatchCmd::Scout { .. } | BatchCmd::Onboard { .. }
            | /* ... ~30 arms ... */ => true,
            _ => false,
        }
    }
}
```

### Replacement
Add an `is_pipeable: bool` flag to each row of `for_each_command!` in `src/cli/registry.rs`. Generate `BatchCmd::is_pipeable(&self)` from the table via a new `gen_is_pipeable_impl!()` macro. Delete the manual match in `commands.rs:325-364`.

(If the BatchCmd enum itself isn't yet driven from the registry, this prompt narrows to "drive `is_pipeable` from the table"; the wider refactor of folding `BatchCmd` into the registry is P2-level and listed as #2.)

---

## P3.27 — `LlmProvider` resolver via registry slice

**Finding:** P3.27
**Files:** `src/llm/mod.rs:200-205, 284-304, 362-398`
**Why:** Three hand-coded match arms (enum, env-var resolve, factory) per provider. Walk a `&[&dyn ProviderRegistry]` slice instead.

### Current code
```rust
// llm/mod.rs:284-304 (resolve)
match std::env::var("CQS_LLM_PROVIDER").as_deref() {
    Ok("anthropic") | Err(_) => LlmProvider::Anthropic,
    Ok("local") => LlmProvider::Local,
    Ok(other) => { tracing::warn!(provider = other, "unknown CQS_LLM_PROVIDER, defaulting to anthropic"); LlmProvider::Anthropic }
}

// llm/mod.rs:362-398 (create_client)
match provider {
    LlmProvider::Anthropic => /* build AnthropicClient */,
    LlmProvider::Local => /* build LocalProvider */,
}
```

### Replacement
Introduce a registry trait + slice:
```rust
pub(crate) trait ProviderRegistry: Sync {
    fn name(&self) -> &'static str;
    fn build(&self, cfg: &LlmConfig) -> Result<Box<dyn BatchProvider>, LlmError>;
}

static PROVIDERS: &[&dyn ProviderRegistry] = &[
    &AnthropicRegistry, &LocalRegistry,
];
```
`resolve()` walks `PROVIDERS.iter().find(|p| p.name() == requested)`; `create_client()` calls the matched `build()`. Add a provider = add one impl + one slice row.

---

## P3.28 — Tree-sitter registry coverage self-test

**Finding:** P3.28
**Files:** `src/language/queries/*.scm`; new test in `src/language/mod.rs`
**Why:** An empty `chunks.scm` `include_str!`s as `""` and silently emits zero chunks. One assertion catches it.

### Replacement
Add to `src/language/mod.rs::tests`:
```rust
#[test]
fn registry_languages_have_nonempty_chunk_query() {
    for lang in REGISTRY.all() {
        if !lang.has_grammar() { continue; }
        assert!(
            !lang.chunk_query.is_empty(),
            "{:?}: chunk_query is empty — silent zero-chunk language",
            lang.lang
        );
    }
}
```
(`has_grammar` may already exist or can be a small helper that returns true when the lang's tree-sitter feature is enabled.)

---

## P3.29 — `find_project_root` markers as a data table

**Finding:** P3.29
**Files:** `src/cli/config.rs:155-162`
**Why:** Hardcoded array works today; converting to a `static` table makes adding Maven / Gradle / .NET / Bazel a one-row change.

### Current code
```rust
let markers = [
    "Cargo.toml", "package.json", "pyproject.toml",
    "setup.py", "go.mod", ".git",
];
for current in path.ancestors() {
    for marker in &markers {
        if current.join(marker).exists() { return Some(current.to_path_buf()); }
    }
}
```

### Replacement
At module top:
```rust
/// (marker filename, label) — label is informational, not used in lookup.
static PROJECT_ROOT_MARKERS: &[(&str, &str)] = &[
    ("Cargo.toml", "rust"),
    ("package.json", "node"),
    ("pyproject.toml", "python"),
    ("setup.py", "python"),
    ("go.mod", "go"),
    (".git", "fallback"),
];
```
Iterate `PROJECT_ROOT_MARKERS.iter().any(|(m, _)| current.join(m).exists())`.

---

## P3.30 — `structural_matchers` shared library

**Finding:** P3.30
**Files:** `src/language/mod.rs:191, 345`; `src/structural.rs`
**Why:** Currently per-language `Option<&[(name, fn)]>`; sharing common matchers (SwallowedException, AsyncIO, Mutex, Unsafe) across Python/JS/Go/Rust requires copying fn bodies.

### Replacement
Define a small set of cross-language matcher functions in `src/structural.rs` keyed by `(Pattern, Language)`:
```rust
pub(crate) static SHARED_MATCHERS: &[(Pattern, Language, StructuralMatcherFn)] = &[
    (Pattern::SwallowedException, Language::Rust, matchers::rust::swallow_exc),
    (Pattern::SwallowedException, Language::Python, matchers::python::swallow_exc),
    // ...
];
```
Have `LanguageDef::structural_matchers` either be derived from this table at lookup time (filter by `lang`), or keep the field but have each `definition_*` row reference the shared fn pointer. Adding a pattern-language pair = one slice row, not a new fn body.

---

## P3.31 — Embedder preset `extras` map (deferred-friendly)

**Finding:** P3.31
**Files:** `src/embedder/models.rs:163-300`
**Why:** New cross-cutting preset attribute fans out to every row; flagging now since presets are still stable but pressure is rising.

### Replacement
Skip if presets are stable. If/when frequent attribute additions land, extend the `define_embedder_presets!` macro grammar with an `extras: { gpu_only = true, expects_bos = true }` block per row that desugars to a `HashMap<&'static str, ModelAttr>` field on `ModelConfig`. Required fields stay positional; sparse/optional ones go through `extras`.

---

## P3.32 — Cache paths use `dirs::cache_dir()` on Windows

**Finding:** P3.32
**Files:** `src/cache.rs:80-84` (`EmbeddingCache::default_path`), `src/cache.rs:1399-1403` (`QueryCache::default_path`), `src/cli/batch/commands.rs:373-376` (query_log)
**Why:** Hardcoded `~/.cache/cqs/...` becomes a hidden `.cache` folder under `C:\Users\X\` on Windows; native is `%LOCALAPPDATA%\cqs\`.

### Current code (pattern at all 3 sites)
```rust
dirs::home_dir()
    .map(|h| h.join(".cache").join("cqs").join("embeddings.db"))
```

### Replacement
```rust
dirs::cache_dir()
    .or_else(|| dirs::home_dir().map(|h| h.join(".cache")))
    .map(|c| c.join("cqs").join("embeddings.db"))
```
Apply the same shape to `QueryCache::default_path` (`query_cache.db`) and the `query_log.jsonl` path in `cli/batch/commands.rs:373-376`.

---

## P3.33 — `dispatch_drift/diff` JSON file fields use `normalize_path`

**Finding:** P3.33
**Files:** `src/suggest.rs:101`, `src/store/types.rs:220`
**Why:** Two more PB-V1.29-5 sites that emit Windows backslashes via `.display().to_string()`.

### Current code
```rust
// src/suggest.rs:101
let file = dead.chunk.file.display().to_string();

// src/store/types.rs:220
let file_display = file.display().to_string();
```

### Replacement
```rust
// src/suggest.rs:101
let file = crate::normalize_path(&dead.chunk.file);

// src/store/types.rs:220
let file_display = crate::normalize_path(file);
```
(If `normalize_path` accepts `&Path` not `&PathBuf`, adjust the borrow.)

---

## P3.34 — `find_ld_library_dir` Windows arm (or doc the gap)

**Finding:** P3.34
**Files:** `src/embedder/provider.rs:115-123`
**Why:** Currently `cfg(target_os="linux")`-gated; no Windows equivalent. Either add one or doc the delegation explicitly.

### Replacement (lower-cost: doc the gap)
Add at the top of `ensure_ort_provider_libs`:
```rust
/// On Linux this walks `LD_LIBRARY_PATH` (`:`-separated) and symlinks ORT
/// provider .so files into the runtime's search dir. On Windows and macOS
/// provider DLL/dylib resolution is delegated entirely to ORT's loader
/// (Windows: `PATH` search; macOS: `DYLD_*` paths). If a future regression
/// surfaces on those platforms, add an arm with `;`-split for `PATH` (Win)
/// or `DYLD_LIBRARY_PATH` (mac).
```
Keep behavior unchanged.

---

## P3.35 — Document `index.lock` advisory-vs-mandatory split

**Finding:** P3.35
**Files:** `src/cli/files.rs:120-213`
**Why:** Same code, two very different concurrency contracts (Linux advisory `flock` vs Windows mandatory `LockFileEx`). Doc + one-time warn so callers can reason about it.

### Replacement
1. Extend the `acquire_index_lock` doc-comment to spell out: Linux/macOS = advisory (non-cqs writers can corrupt); Windows = mandatory (third-party tools may see "sharing violation"); WSL `/mnt/c/` follows Linux semantics for the call but Windows for the underlying file.
2. On Windows only, add a one-shot `tracing::warn!` (gated on `OnceLock`) at first lock acquisition:
```rust
#[cfg(windows)]
{
    static WARNED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    WARNED.get_or_init(|| {
        tracing::warn!(
            "index.lock is mandatory on Windows — third-party tools opening \
             index.db may fail with sharing violations while cqs is running"
        );
    });
}
```

---

## P3.36 — `is_wsl_drvfs_path` matches UNC + uppercase drives

**Finding:** P3.36
**Files:** `src/config.rs:92-101`
**Why:** Misses `//wsl.localhost/`, `//wsl$/`, and uppercase drive letters when WSL `automount.options=case=force` is set.

### Current code
```rust
pub fn is_wsl_drvfs_path(path: &Path) -> bool {
    let s = path.to_string_lossy();
    s.starts_with("/mnt/")
        && s.chars().nth(5).is_some_and(|c| c.is_ascii_lowercase())
        && s.chars().nth(6) == Some('/')
}
```

### Replacement
```rust
pub fn is_wsl_drvfs_path(path: &Path) -> bool {
    let s = path.to_string_lossy();
    // Standard /mnt/<letter>/ — accept upper or lowercase
    if let Some(rest) = s.strip_prefix("/mnt/") {
        if let (Some(c), Some('/')) = (rest.chars().next(), rest.chars().nth(1)) {
            if c.is_ascii_alphabetic() { return true; }
        }
    }
    // UNC paths reaching back into WSL
    if s.starts_with("//wsl.localhost/") || s.starts_with("//wsl$/") {
        return true;
    }
    false
}
```

---

## P3.37 — `blame.rs` git_file via `normalize_slashes`

**Finding:** P3.37
**Files:** `src/cli/commands/io/blame.rs:113-115`
**Why:** Current `replace('\\', "/")` only handles backslashes; verbatim `\\?\` prefix slips through, breaking `git log --format=... -- <file>`.

### Current code
```rust
let git_file = rel_file.replace('\\', "/");
```

### Replacement
```rust
let git_file = crate::normalize_slashes(&rel_file);
```

---

## P3.38 — Daemon socket-path: warn on `XDG_RUNTIME_DIR` unset (Linux only)

**Finding:** P3.38
**Files:** `src/daemon_translate.rs:179-188`
**Why:** Silent fallback to `/tmp` (mode 1777) hides a meaningful trust drop on multi-user Linux. macOS `/var/folders/...` is fine.

### Current code
```rust
fn daemon_socket_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    dir.join(/* per-user socket name */)
}
```

### Replacement
```rust
fn daemon_socket_path() -> PathBuf {
    if let Some(d) = std::env::var_os("XDG_RUNTIME_DIR") {
        return PathBuf::from(d).join(/* socket name */);
    }
    #[cfg(target_os = "linux")]
    {
        static WARNED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
        WARNED.get_or_init(|| {
            tracing::info!(
                "XDG_RUNTIME_DIR unset — daemon socket falls back to temp_dir; \
                 consider XDG_RUNTIME_DIR=/run/user/$(id -u)"
            );
        });
    }
    std::env::temp_dir().join(/* socket name */)
}
```

---

## P3.39 — `write_slot_model` / `write_active_slot` use `atomic_replace`

**Finding:** P3.39
**Files:** `src/slot/mod.rs:237-277` (`write_slot_model`), `src/slot/mod.rs:363-406` (`write_active_slot`)
**Why:** Bespoke temp+rename without parent-dir fsync. `crate::fs::atomic_replace` already does this for `notes.toml` / `audit-mode.json`.

### Current code (sketch — both functions)
```rust
let tmp = dir.join(format!(".slot.toml.{}.tmp", temp_suffix()));
let mut f = fs::File::create(&tmp)?;
f.write_all(contents.as_bytes())?;
f.sync_all()?;
fs::rename(&tmp, &final_path)?;
// no parent-dir fsync
```

### Replacement
```rust
crate::fs::atomic_replace(&final_path, contents.as_bytes())?;
```
Drop the bespoke temp+rename block from each function. ~20 LOC each.

---

## P3.40 — `update_umap_coords_batch`: DROP TEMP TABLE before CREATE

**Finding:** P3.40
**Files:** `src/store/chunks/crud.rs:392-450`
**Why:** TEMP TABLE is connection-scoped, not transaction-scoped; rollback can leave a stale `_update_umap` between calls on the same pooled connection.

### Current code
```rust
sqlx::query("CREATE TEMP TABLE IF NOT EXISTS _update_umap (...)").execute(&mut *tx).await?;
sqlx::query("DELETE FROM _update_umap").execute(&mut *tx).await?;
// ... INSERT INTO _update_umap, UPDATE chunks FROM _update_umap, DROP TABLE IF EXISTS _update_umap;
```

### Replacement
```rust
sqlx::query("DROP TABLE IF EXISTS _update_umap").execute(&mut *tx).await?;
sqlx::query("CREATE TEMP TABLE _update_umap (...)").execute(&mut *tx).await?;
// no DELETE needed; INSERT INTO _update_umap proceeds cleanly
```

---

## P3.41 — `reindex_files`: build embeddings without placeholders

**Finding:** P3.41
**Files:** `src/cli/watch.rs:2918-2924`
**Why:** Allocates N empty `Embedding::new(vec![])` placeholders then overwrites each — pure waste plus a future zero-norm-vector landmine.

### Current code
```rust
let mut embeddings: Vec<Embedding> = vec![Embedding::new(vec![]); chunk_count];
for (i, e) in cached { embeddings[i] = e; }
for (i, e) in new_embeddings { embeddings[i] = e; }
```

### Replacement
Mirror the bulk-pipeline pattern (`src/cli/pipeline/embedding.rs::create_embedded_batch`):
```rust
let mut by_index: HashMap<usize, Embedding> = HashMap::with_capacity(chunk_count);
for (i, e) in cached { by_index.insert(i, e); }
for (i, e) in new_embeddings { by_index.insert(i, e); }
let embeddings: Vec<Embedding> = (0..chunk_count)
    .map(|i| by_index.remove(&i)
        .unwrap_or_else(|| panic!("missing embedding at index {i}")))
    .collect();
```
(Or refactor to call `create_embedded_batch` directly if its signature fits.)

---

## P3.42 — `prepare_for_embedding`: skip store query when global cache satisfies all

**Finding:** P3.42
**Files:** `src/cli/pipeline/embedding.rs:64-82`
**Why:** Always issues `store.get_embeddings_by_hashes` even when global cache has every entry; one wasted bind-heavy SELECT per warm reindex.

### Current code (sketch)
```rust
let global_hits = global_cache.read_batch(&hashes, ...)?;
let store_hits = store.get_embeddings_by_hashes(&hashes)?; // unconditional
```

### Replacement
```rust
let global_hits = global_cache.read_batch(&hashes, ...)?;
let missed: Vec<&str> = hashes.iter()
    .filter(|h| !global_hits.contains_key(h.as_str()))
    .map(|h| h.as_str()).collect();
let store_hits = if missed.is_empty() {
    HashMap::new()
} else {
    store.get_embeddings_by_hashes(&missed)?
};
```

---

## P3.43 — Daemon socket: fold args validation + extraction into one pass

**Finding:** P3.43
**Files:** `src/cli/watch.rs:266-297`
**Why:** Every daemon query walks the args array twice (validation + extraction). Single pass collects both.

### Current code
```rust
let bad_arg_indices: Vec<usize> = arr.iter().enumerate()
    .filter_map(|(i, v)| (!v.is_string()).then_some(i)).collect();
if !bad_arg_indices.is_empty() { /* reject */ }
let args: Vec<String> = arr.iter()
    .filter_map(|v| v.as_str().map(String::from)).collect();
```

### Replacement
```rust
let mut args = Vec::with_capacity(arr.len());
let mut bad_arg_indices = Vec::new();
for (i, v) in arr.iter().enumerate() {
    match v.as_str() {
        Some(s) => args.push(s.to_string()),
        None => bad_arg_indices.push(i),
    }
}
if !bad_arg_indices.is_empty() { /* reject as before */ }
```

---

## P3.44 — `build_graph` edge dedup with hashed key

**Finding:** P3.44
**Files:** `src/serve/data.rs:367-373`
**Why:** `(file.clone(), caller.clone(), callee.clone())` allocates 3 Strings per row even on dedup miss. Hash to u64 instead.

### Current code
```rust
let key = (file.clone(), caller_name.clone(), callee_name.clone());
if seen.insert(key) {
    accum.push((file, caller_name, callee_name));
}
```

### Replacement
```rust
use std::hash::{Hash, Hasher};
use std::collections::hash_map::DefaultHasher;
let mut h = DefaultHasher::new();
file.hash(&mut h); caller_name.hash(&mut h); callee_name.hash(&mut h);
if seen.insert(h.finish()) {
    accum.push((file, caller_name, callee_name));
}
```
Change `seen` type from `HashSet<(String,String,String)>` to `HashSet<u64>`.

---

## P3.45 — `extract_imports`: borrow keys, allocate only on accept

**Finding:** P3.45
**Files:** `src/where_to_add.rs:258-276`
**Why:** `seen.insert(trimmed.to_string())` per candidate line allocates even on rejection.

### Current code
```rust
let mut seen: HashSet<String> = HashSet::new();
let mut imports: Vec<String> = Vec::new();
for chunk in chunks {
    for line in chunk.content.lines() {
        let trimmed = line.trim();
        for &prefix in prefixes {
            if trimmed.starts_with(prefix) && imports.len() < max
                && seen.insert(trimmed.to_string()) {
                imports.push(trimmed.to_string());
                break;
            }
        }
    }
}
```

### Replacement
```rust
let mut seen: HashSet<&str> = HashSet::new();
let mut imports: Vec<String> = Vec::new();
for chunk in chunks {
    for line in chunk.content.lines() {
        let trimmed = line.trim();
        for &prefix in prefixes {
            if trimmed.starts_with(prefix) && imports.len() < max
                && seen.insert(trimmed) {
                imports.push(trimmed.to_string());
                break;
            }
        }
    }
}
```

---

## P3.46 — Watch reindex: `existing.remove` instead of `.get().clone()`

**Finding:** P3.46
**Files:** `src/cli/watch.rs:2879-2887`
**Why:** Per cache hit clones a 4 KB `Embedding` (1024-dim). `.remove()` takes ownership.

### Current code
```rust
let existing = store.get_embeddings_by_hashes(&hashes)?;
for (i, chunk) in chunks.iter().enumerate() {
    if let Some(emb) = existing.get(&chunk.content_hash) {
        cached.push((i, emb.clone()));
    } else {
        to_embed.push((i, chunk));
    }
}
```

### Replacement
```rust
let mut existing = store.get_embeddings_by_hashes(&hashes)?;
for (i, chunk) in chunks.iter().enumerate() {
    if let Some(emb) = existing.remove(&chunk.content_hash) {
        cached.push((i, emb));
    } else {
        to_embed.push((i, chunk));
    }
}
```

---

## P3.47 — `LocalProvider` worker thread stack 512 KB

**Finding:** P3.47
**Files:** `src/llm/local.rs:163-256`
**Why:** Default 2 MB stack × concurrency=64 = 128 MB just for the fan-out. Worker body is shallow.

### Current code (sketch)
```rust
std::thread::scope(|s| {
    for _ in 0..workers {
        s.spawn(|| { /* recv → http → parse → mutex.insert */ });
    }
});
```

### Replacement
Switch to `Builder`-based threads with manual join, since `std::thread::scope::Scope::spawn` lacks a stack-size hook:
```rust
let mut handles = Vec::with_capacity(workers);
for i in 0..workers {
    let h = std::thread::Builder::new()
        .name(format!("cqs-llm-worker-{i}"))
        .stack_size(512 * 1024)
        .spawn(/* worker closure */)?;
    handles.push(h);
}
for h in handles { let _ = h.join(); }
```
Adjust the captured borrows to be `Arc`-cloned since we're outside `scope`. Drop the upper concurrency clamp from 64 to 16 in `local_concurrency()` while you're there.

---

## P3.48 — `LocalProvider::http`: cap idle pool

**Finding:** P3.48
**Files:** `src/llm/local.rs:97-100`
**Why:** Default reqwest pool = unbounded idle, 90 s timeout. Long indexing runs leak idle slots into vLLM/llama.cpp.

### Current code
```rust
Client::builder()
    .timeout(timeout)
    .redirect(Policy::none())
    .build()?
```

### Replacement
```rust
Client::builder()
    .timeout(timeout)
    .redirect(Policy::none())
    .pool_max_idle_per_host(self.concurrency)
    .pool_idle_timeout(Duration::from_secs(30))
    .build()?
```

---

## P3.49 — `cmd_similar` (CLI) integration test

**Finding:** P3.49
**Files:** `src/cli/commands/search/similar.rs:41`
**Why:** Library `find_similar` is tested; the CLI wrapper (target lookup + pattern filter + JSON build) is not.

### Replacement
Add to `src/cli/commands/search/similar.rs::tests` (or new `tests/cli_similar_test.rs` if `cmd_similar` is hard to drive in-process):
```rust
#[test]
fn cmd_similar_returns_other_seeded_chunks() {
    let fix = common::InProcessFixture::new();
    fix.seed_chunks(&[("foo", "fn foo() { bar(); }"),
                      ("bar", "fn bar() { 1+1; }"),
                      ("baz", "fn baz() { unrelated(); }")]);
    let mut sink = Vec::<u8>::new();
    let ctx = fix.command_context_with_json_sink(&mut sink);
    cmd_similar(&ctx, "foo", /*limit*/ 2).unwrap();
    let json: serde_json::Value = serde_json::from_slice(&sink).unwrap();
    let names: Vec<&str> = json["data"]["results"].as_array().unwrap().iter()
        .map(|r| r["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"bar"));
    assert!(!names.contains(&"foo")); // self excluded
}
```

---

## P3.50 — `cmd_ci` happy-path test

**Finding:** P3.50
**Files:** `src/cli/commands/review/ci.rs:9` + `tests/cli_train_review_test.rs` (or new `tests/cli_ci_test.rs`)
**Why:** Existing tests cover error paths only; markdown formatting + exit-code mapping for a real diff are unpinned.

### Replacement
Add a test that feeds a real unified diff touching a hotspot function in an `InProcessFixture`-seeded corpus:
```rust
#[test]
fn cmd_ci_high_risk_diff_emits_high_risk_section_and_nonzero_exit() {
    let fix = common::InProcessFixture::new();
    fix.seed_chunks(&[("hotspot", "fn hotspot() { /* many callers */ }")]);
    fix.seed_callers("hotspot", 12); // synthetic call edges
    let diff = "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,3 +1,3 @@\n \
                fn hotspot() {\n-    do_old();\n+    do_new();\n }\n";
    let mut stdout = Vec::<u8>::new();
    let exit = cmd_ci_with_input(&fix.context(), diff, &mut stdout).unwrap();
    let out = String::from_utf8(stdout).unwrap();
    assert!(out.contains("High-risk"));
    assert_ne!(exit, 0);
}
```

---

## P3.51 — `cmd_gather` (CLI) integration test

**Finding:** P3.51
**Files:** `src/cli/commands/search/gather.rs:77`
**Why:** Library `gather()` is tested; CLI-only steps (`--max-files` clamp, content injection, token-budget trim) are not.

### Replacement
Add `tests/cli_gather_test.rs`:
```rust
#[test]
fn cli_gather_clamps_max_files_and_injects_content() {
    let fix = common::InProcessFixture::new();
    fix.seed_chunks(&[/* 5 chunks across 5 files */]);
    let out = cqs(&["gather", "needle", "--json", "--max-files", "2"]).output();
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let groups = json["data"]["results"].as_array().unwrap();
    assert_eq!(groups.len(), 2);                              // clamp
    assert!(groups[0]["content"].as_str().unwrap().len() > 0); // content injected
}
```

---

## P3.52 — `dispatch_line` happy-path test for a valid command

**Finding:** P3.52
**Files:** `src/cli/batch/mod.rs:557` (`dispatch_line`); test block in same file
**Why:** Existing tests are all error/adversarial. A success-envelope shape regression would slip through.

### Replacement
Add to `src/cli/batch/mod.rs::tests`:
```rust
#[test]
fn test_dispatch_line_stats_emits_success_envelope() {
    let fix = common::InProcessFixture::new();
    let mut ctx = fix.batch_context();
    let mut sink = Vec::<u8>::new();
    ctx.dispatch_line("stats", &mut sink).unwrap();
    let line = std::str::from_utf8(&sink).unwrap().lines().next().unwrap();
    let v: serde_json::Value = serde_json::from_str(line).unwrap();
    assert!(v["error"].is_null());
    assert!(v["data"]["total_chunks"].is_number());
}
```

---

## P3.53 — `select_provider` / `detect_provider` direct tests

**Finding:** P3.53
**Files:** `src/embedder/provider.rs:171, 188, 258`; new `#[cfg(test)] mod tests` (overlaps P3.16; consolidate)
**Why:** #1120's provider split has zero tests for the cache-on-first-call invariant or the priority list.

### Replacement
Add to `src/embedder/provider.rs::tests` (alongside the P3.16 tests):
```rust
#[test]
fn select_provider_caches_first_call() {
    // OnceCell semantics — first call wins, second returns same value.
    let p1 = select_provider();
    let p2 = select_provider();
    assert_eq!(format!("{p1:?}"), format!("{p2:?}"));
}

#[cfg(not(any(feature = "cuda-index", feature = "ep-coreml", feature = "ep-rocm")))]
#[test]
fn detect_provider_returns_cpu_when_no_features() {
    // Pure-CPU build must yield the CPU branch.
    assert!(matches!(detect_provider(), ExecutionProvider::CPU));
}
```
(Add a `cuda-index`-gated test that asserts CUDA selection on a CUDA build if the runtime has a GPU; otherwise skip.)

---

# P4 — GitHub Issues (paste-ready)

## P4.1 — AuthToken alphabet invariant via type-state

**Disposition:** GitHub issue
**Files:** `src/serve/auth.rs:75-78` (`from_string`), `src/serve/auth.rs:218-220` (`HeaderValue::from_str(&cookie).expect(…)`)

### Issue body (paste-ready)

**Title:** auth: harden AuthToken alphabet invariant via type-state, not docstring

**Body:**

> `AuthToken::from_string` is currently `#[cfg(test)]` so production code cannot construct a token with arbitrary bytes. The contract that the alphabet is URL-safe base64 (and that `HeaderValue::from_str(&cookie)` therefore cannot fail) is enforced only by the docstring on `random()`. The `.expect(…)` at `auth.rs:218` is a real panic if the invariant ever fails.
>
> If a future feature lifts the cfg-gate (e.g. "stable token from env var" for scripted automation) and the env var contains CR/LF, `;`, or `,`, the worker panics on every redirect or smuggles a second cookie pair into the `Set-Cookie` header. Today the path is unreachable from outside tests, but the type does not enforce the invariant; one cfg-gate change is all it takes.
>
> **Why deferred:** a clean fix is a type-state refactor — wrap the inner `String` in a `pub struct AuthToken(String)` newtype constructed only via `random()` / a validated `from_str_validated`. Both constructors verify `[A-Za-z0-9_-]{32,}` and panic at construction, not at use. That changes the `pub` surface of the `auth` module and ripples into every test that builds tokens by hand.
>
> **Pointers:**
> - construction sites: `src/serve/auth.rs:75-78` (`from_string`), `src/serve/auth.rs:60-72` (`random`)
> - panic site that would convert into a real safety proof: `src/serve/auth.rs:218-220`
> - test usage to migrate: search `AuthToken::from_string` under `src/serve/`

---

## P4.2 — Path=/ cookie scope on 127.0.0.1 — multi-instance hijack

**Disposition:** GitHub issue
**Files:** `src/serve/auth.rs:211-214` (`Set-Cookie: cqs_token={token}; Path=/; HttpOnly; SameSite=Strict`)

### Issue body (paste-ready)

**Title:** serve: localhost cookie collision when running multiple `cqs serve` instances

**Body:**

> Browsers scope cookies by `(host, path)` but **not by port**. Two `cqs serve` instances on the same machine but different ports (project A on 8080, project B on 8081) both set `cqs_token` with `Path=/`. Authenticating to one overwrites the cookie set by the other; the previously-authenticated tab silently 401s on every navigation, and (worse) any link sends the wrong token to the wrong server.
>
> Threat model: an attacker who can convince a victim to run `cqs serve` against a malicious project on a port the attacker controls can drop any cookie they like into the victim's localhost cookie jar — combined with `SameSite=Strict`-bypass via top-level navigation, this is a real cookie-jar overwrite vector.
>
> **Steps:**
> 1. Run `cqs serve --port 8080` in project A; auth via the browser banner.
> 2. Run `cqs serve --port 8081` in project B; auth via the browser banner.
> 3. Reload project A's tab — every request now 401s because the project B token clobbered it.
>
> **Why deferred:** clean fixes all have trade-offs. `__Host-` cookie prefix requires `Secure`, which loopback HTTP doesn't satisfy. Port-suffixed cookie names (`cqs_token_8080`) increase knob count and break URL-bar bookmarks across launches when the port floats. Path-rewriting (`Path=/api/__cqs_<port>/`) is heavy. Pragmatic option: derive cookie name from a hash of `(bind_addr, launch_time)` so two instances don't collide and a new launch invalidates the old cookie — but that changes the CLI launch banner (the printed token URL must include the cookie-name salt) and requires a one-time UX audit.
>
> **Pointers:**
> - cookie set: `src/serve/auth.rs:211-214`
> - cookie read: `src/serve/auth.rs:158-172` (`check_request`)
> - launch-banner path: `src/serve/mod.rs:111-117`
> - existing acknowledgement of the issue: `src/serve/auth.rs:42-47`

---

## P4.3 — `Option<AuthToken>` permits silent no-auth router

**Disposition:** GitHub issue
**Files:** `src/serve/mod.rs:78-83` (`run_server` signature), `src/serve/mod.rs:154-178` (`build_router`)

### Issue body (paste-ready)

**Title:** serve: type-state `AuthMode` to prevent silently building a no-auth router

**Body:**

> `run_server` and `build_router` both take `auth: Option<AuthToken>`. Passing `None` silently disables auth — there is no compile-time gate. The `cmd_serve` entry path correctly defaults to `Some(random())` and only opts into `None` on `--no-auth`, but the type does not constrain future internal callers (an embedded smoke test, a feature-gated dev mode, an alternate CLI surface) from passing `None` and shipping a fully open server. The only runtime signal is an `eprintln!` gated on `quiet == false` (`mod.rs:120-123`).
>
> Today `run_server(store, addr, /* quiet */ true, /* auth */ None)` ships a wide-open server with zero output. The "default secure" property lives in convention, not in the type system. The equivalent invariant in `cqs ref add` is enforced by validate-by-construction; here it is not.
>
> **Why deferred:** the clean fix is a type-state refactor — replace `Option<AuthToken>` with `enum AuthMode { Required(AuthToken), Disabled { ack: NoAuthAcknowledgement } }` where `NoAuthAcknowledgement` is a `pub` zero-sized type only constructable inside `cmd_serve` after the `--no-auth` flag is parsed. Future internal callers cannot instantiate the disabled variant without intentionally importing the proof type. This ripples into every test that builds a router today via `build_router(.., None)` and the public-ish `run_server` signature.
>
> **Cheaper interim mitigation** (one-line, can land before the refactor): in the `None` arm of `build_router` add a `tracing::error!("serve: AUTH DISABLED — request layer is open")` so any caller (test or future) shows up loudly in journald. Not a substitute for the type-state fix; just blast-radius limiter.
>
> **Pointers:**
> - signature: `src/serve/mod.rs:78-83`
> - router construction: `src/serve/mod.rs:154-178`
> - the only caller that legitimately passes `None`: `src/cli/commands/serve.rs:61-65` (after `--no-auth` parse)
> - banner suppression: `src/serve/mod.rs:120-123`

---
