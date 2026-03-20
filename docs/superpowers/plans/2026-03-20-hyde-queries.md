# SQ-12: HyDE Query Predictions Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Generate predicted search queries for each function at index time via LLM, embedding them alongside the NL description to close the gap between user queries and indexed content.

**Architecture:** New `--hyde-queries` flag on `cqs index` triggers a Batches API pass (same pattern as `--llm-summaries`). Predictions stored in `llm_summaries` table with `purpose="hyde"`. The existing enrichment pass is extended to incorporate predictions into NL before re-embedding. Zero changes to the search pipeline.

**Tech Stack:** Rust, Claude Batches API (haiku), ONNX Runtime (E5-base-v2)

**Spec:** `docs/superpowers/specs/2026-03-20-hyde-queries-design.md`

---

## File Map

| File | Responsibility |
|------|---------------|
| `src/llm.rs` | `build_hyde_prompt`, `submit_hyde_batch`, `hyde_query_pass` |
| `src/nl.rs` | Extend `generate_nl_with_call_context_and_summary` with `hyde` param |
| `src/store/mod.rs` | `set_pending_hyde_batch_id` / `get_pending_hyde_batch_id` |
| `src/cli/enrichment.rs` | Pre-fetch hyde predictions, include in enrichment hash, pass to NL |
| `src/cli/commands/index.rs` | `--hyde-queries` and `--max-hyde` CLI flags, call `hyde_query_pass` |
| `src/lib.rs` | Re-export `hyde_query_pass` (if needed) |

---

### Task 1: Pending batch metadata for hyde

**Files:**
- Modify: `src/store/mod.rs:778-842`

- [ ] **Step 1: Add `set_pending_hyde_batch_id` and `get_pending_hyde_batch_id`**

Copy the existing `set_pending_doc_batch_id` / `get_pending_doc_batch_id` pair (lines 811-842), change the metadata key from `'pending_doc_batch'` to `'pending_hyde_batch'`.

```rust
/// Store a pending hyde batch ID so interrupted processes can resume polling.
pub fn set_pending_hyde_batch_id(&self, batch_id: Option<&str>) -> Result<(), StoreError> {
    self.rt.block_on(async {
        match batch_id {
            Some(id) => {
                sqlx::query(
                    "INSERT OR REPLACE INTO metadata (key, value) VALUES ('pending_hyde_batch', ?1)",
                )
                .bind(id)
                .execute(&self.pool)
                .await?;
            }
            None => {
                sqlx::query("DELETE FROM metadata WHERE key = 'pending_hyde_batch'")
                    .execute(&self.pool)
                    .await?;
            }
        }
        Ok(())
    })
}

/// Get the pending hyde batch ID, if any.
pub fn get_pending_hyde_batch_id(&self) -> Result<Option<String>, StoreError> {
    self.rt.block_on(async {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT value FROM metadata WHERE key = 'pending_hyde_batch'")
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.map(|(v,)| v))
    })
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test --features gpu-index --lib store -- --nocapture 2>&1 | tail -5
```

- [ ] **Step 3: Commit**

```
feat(store): pending hyde batch metadata for resume support
```

---

### Task 2: Hyde prompt and batch submission

**Files:**
- Modify: `src/llm.rs`

- [ ] **Step 1: Add `build_hyde_prompt`**

Add after `build_doc_prompt` (around line 417). The prompt asks the LLM to predict search queries for a function.

```rust
/// Build the prompt for hyde query prediction.
///
/// Asks the LLM to predict 3-5 search queries a developer would type
/// to find this function. Doc comments are deliberately excluded to
/// prevent parroting — we want novel query angles from the code itself.
fn build_hyde_prompt(content: &str, signature: &str, language: &str) -> String {
    format!(
        "You are a code search query predictor. Given a function, output 3-5 short \
         search queries a developer would type to find this function. One query per \
         line. No numbering, no explanation. Queries should be natural language, \
         not code.\n\nLanguage: {language}\nSignature: {signature}\n\n{content}"
    )
}
```

Note: Unlike `build_prompt`/`build_doc_prompt` which take `chunk_type`, this takes `signature` instead — the signature is the key discriminating feature for query prediction.

- [ ] **Step 2: Add `submit_hyde_batch`**

Copy `submit_doc_batch` (lines 419-471), change `build_doc_prompt` to `build_hyde_prompt` and the log message.

```rust
fn submit_hyde_batch(
    &self,
    items: &[(String, String, String, String)],
    max_tokens: u32,
) -> Result<String, LlmError> {
    let model = self.llm_config.model.clone();
    let requests: Vec<BatchItem> = items
        .iter()
        .map(|(id, content, signature, language)| BatchItem {
            custom_id: id.clone(),
            params: MessagesRequest {
                model: model.clone(),
                max_tokens,
                messages: vec![ChatMessage {
                    role: "user".to_string(),
                    content: Self::build_hyde_prompt(content, signature, language),
                }],
            },
        })
        .collect();

    let url = format!("{}/messages/batches", self.llm_config.api_base);
    let response = self
        .http
        .post(&url)
        .header("x-api-key", &self.api_key)
        .header("anthropic-version", API_VERSION)
        .header("content-type", "application/json")
        .json(&BatchRequest { requests })
        .send()?;

    let status = response.status();
    if status == 401 {
        return Err(LlmError::Api {
            status: 401,
            message: "Invalid ANTHROPIC_API_KEY (401 Unauthorized)".to_string(),
        });
    }
    if !status.is_success() {
        let body = response.text().unwrap_or_default();
        let message = serde_json::from_str::<ApiError>(&body)
            .map(|err| format!("Hyde batch submission failed: {}", err.error.message))
            .unwrap_or_else(|_| {
                format!("Hyde batch submission failed: HTTP {status}: {body}")
            });
        return Err(LlmError::Api {
            status: status.as_u16(),
            message,
        });
    }

    let batch: BatchResponse = response.json()?;
    tracing::info!(batch_id = %batch.id, count = items.len(), "Hyde batch submitted");
    Ok(batch.id)
}
```

- [ ] **Step 3: Add `resume_or_fetch_hyde_batch`**

Add a new free function after `resume_or_fetch_batch` (line 507). Same logic but uses purpose `"hyde"` and `set_pending_hyde_batch_id`.

```rust
/// Wait for a hyde batch to complete, fetch results, store them, and clear the pending marker.
fn resume_or_fetch_hyde_batch(
    client: &Client,
    store: &Store,
    batch_id: &str,
    quiet: bool,
) -> Result<usize, LlmError> {
    client.wait_for_batch(batch_id, quiet)?;

    if !quiet {
        eprintln!();
    }

    let results = client.fetch_batch_results(batch_id)?;

    let model = client.llm_config.model.clone();
    let hyde_entries: Vec<(String, String, String, String)> = results
        .into_iter()
        .map(|(hash, predictions)| (hash, predictions, model.clone(), "hyde".to_string()))
        .collect();
    let count = hyde_entries.len();
    if !hyde_entries.is_empty() {
        store.upsert_summaries_batch(&hyde_entries)?;
    }

    if let Err(e) = store.set_pending_hyde_batch_id(None) {
        tracing::warn!(error = %e, "Failed to clear pending hyde batch ID");
    }

    Ok(count)
}
```

- [ ] **Step 4: Compile check**

```bash
cargo test --features gpu-index --lib llm -- --no-run 2>&1 | tail -5
```

- [ ] **Step 5: Commit**

```
feat(llm): hyde prompt builder and batch submission
```

---

### Task 3: `hyde_query_pass` top-level function

**Files:**
- Modify: `src/llm.rs`

- [ ] **Step 1: Add `hyde_query_pass`**

Add after `doc_comment_pass`. Mirrors `llm_summary_pass` structure: scan chunks, skip those with existing hyde entries, queue for batch, submit, fetch results. Key difference: no doc-comment shortcut (every callable chunk needs LLM predictions).

```rust
/// Run the hyde query prediction pass using the Batches API.
///
/// For each callable chunk without existing hyde predictions, asks the LLM
/// to predict 3-5 search queries a developer would type to find this function.
/// Results stored with purpose="hyde" in llm_summaries table.
///
/// Returns the number of new predictions generated.
pub fn hyde_query_pass(
    store: &Store,
    quiet: bool,
    config: &crate::config::Config,
    max_hyde: usize,
) -> Result<usize, LlmError> {
    let _span = tracing::info_span!("hyde_query_pass").entered();

    let llm_config = LlmConfig::resolve(config);
    tracing::info!(
        model = %llm_config.model,
        api_base = %llm_config.api_base,
        "Hyde query pass starting"
    );

    let api_key = std::env::var("ANTHROPIC_API_KEY").map_err(|_| {
        LlmError::ApiKeyMissing(
            "--hyde-queries requires ANTHROPIC_API_KEY environment variable".to_string(),
        )
    })?;
    let client = Client::new(&api_key, llm_config)?;

    let mut cached = 0usize;
    let mut skipped = 0usize;
    let mut cursor = 0i64;
    const PAGE_SIZE: usize = 500;

    let mut batch_items: Vec<(String, String, String, String)> = Vec::new();
    let mut queued_hashes: std::collections::HashSet<String> = std::collections::HashSet::new();

    let stats = store.stats()?;
    tracing::info!(chunks = stats.total_chunks, "Scanning for hyde predictions");

    let mut batch_full = false;
    let effective_max = if max_hyde > 0 {
        max_hyde.min(MAX_BATCH_SIZE)
    } else {
        MAX_BATCH_SIZE
    };

    loop {
        let (chunks, next) = store.chunks_paged(cursor, PAGE_SIZE)?;
        if chunks.is_empty() {
            break;
        }
        cursor = next;

        let hashes: Vec<&str> = chunks.iter().map(|c| c.content_hash.as_str()).collect();
        let existing = store.get_summaries_by_hashes(&hashes, "hyde")?;

        for cs in &chunks {
            if existing.contains_key(&cs.content_hash) {
                cached += 1;
                continue;
            }

            if !cs.chunk_type.is_callable() {
                skipped += 1;
                continue;
            }

            if cs.content.len() < MIN_CONTENT_CHARS {
                skipped += 1;
                continue;
            }

            if cs.window_idx.is_some_and(|idx| idx > 0) {
                skipped += 1;
                continue;
            }

            if queued_hashes.insert(cs.content_hash.clone()) {
                batch_items.push((
                    cs.content_hash.clone(),
                    if cs.content.len() > MAX_CONTENT_CHARS {
                        cs.content[..cs.content.floor_char_boundary(MAX_CONTENT_CHARS)].to_string()
                    } else {
                        cs.content.clone()
                    },
                    cs.signature.clone(), // signature for hyde prompt (not chunk_type)
                    cs.language.to_string(),
                ));
                if batch_items.len() >= effective_max {
                    batch_full = true;
                    break;
                }
            }
        }
        if batch_full {
            break;
        }
    }

    tracing::info!(cached, skipped, api_needed = batch_items.len(), "Hyde scan complete");

    if !quiet && !batch_items.is_empty() {
        eprintln!(
            "Generating hyde predictions for {} functions...",
            batch_items.len()
        );
    }

    // Submit batch or resume pending
    const HYDE_MAX_TOKENS: u32 = 150;

    let api_generated = if batch_items.is_empty() {
        match store.get_pending_hyde_batch_id() {
            Ok(Some(pending)) => {
                tracing::info!(batch_id = %pending, "Resuming pending hyde batch");
                resume_or_fetch_hyde_batch(&client, store, &pending, quiet)?
            }
            _ => 0,
        }
    } else {
        let batch_id = match store.get_pending_hyde_batch_id() {
            Ok(Some(pending)) => {
                match client.check_batch_status(&pending) {
                    Ok(status)
                        if status == "in_progress"
                            || status == "finalizing"
                            || status == "created"
                            || status == "ended" =>
                    {
                        tracing::info!(batch_id = %pending, status = %status, "Resuming pending hyde batch");
                        pending
                    }
                    _ => {
                        let id = client.submit_hyde_batch(&batch_items, HYDE_MAX_TOKENS)?;
                        if let Err(e) = store.set_pending_hyde_batch_id(Some(&id)) {
                            tracing::warn!(error = %e, "Failed to store pending hyde batch ID");
                        }
                        id
                    }
                }
            }
            _ => {
                let id = client.submit_hyde_batch(&batch_items, HYDE_MAX_TOKENS)?;
                if let Err(e) = store.set_pending_hyde_batch_id(Some(&id)) {
                    tracing::warn!(error = %e, "Failed to store pending hyde batch ID");
                }
                id
            }
        };

        resume_or_fetch_hyde_batch(&client, store, &batch_id, quiet)?
    };

    tracing::info!(api_generated, cached, skipped, "Hyde query pass complete");

    // Don't print here — caller (cmd_index) handles user-facing output,
    // matching the llm_summary_pass pattern.

    Ok(api_generated)
}
```

- [ ] **Step 2: Compile check**

```bash
cargo check --features gpu-index 2>&1 | tail -5
```

- [ ] **Step 3: Commit**

```
feat(llm): hyde_query_pass — batch LLM prediction of search queries
```

---

### Task 4: Extend NL assembly with hyde predictions

**Files:**
- Modify: `src/nl.rs:280-335`

- [ ] **Step 1: Add `hyde` parameter to `generate_nl_with_call_context_and_summary`**

```rust
/// Generate NL with call context, optional LLM summary (SQ-6), and optional
/// hyde query predictions (SQ-12).
pub fn generate_nl_with_call_context_and_summary(
    chunk: &Chunk,
    ctx: &CallContext,
    callee_doc_freq: &std::collections::HashMap<String, f32>,
    max_callers: usize,
    max_callees: usize,
    summary: Option<&str>,
    hyde: Option<&str>,
) -> String {
    // ... existing body unchanged until the end ...

    // Prepend LLM summary if available (SQ-6)
    let nl = match summary {
        Some(s) if !s.is_empty() => format!("{} {}", s, nl),
        _ => nl,
    };

    // Append hyde query predictions (SQ-12)
    match hyde {
        Some(h) if !h.is_empty() => {
            // Convert newline-separated predictions to comma-joined
            let queries: String = h
                .lines()
                .map(|l| l.trim())
                .filter(|l| !l.is_empty())
                .collect::<Vec<_>>()
                .join(", ");
            if queries.is_empty() {
                nl
            } else {
                format!("{}. Queries: {}", nl, queries)
            }
        }
        _ => nl,
    }
}
```

- [ ] **Step 2: Update `generate_nl_with_call_context` wrapper**

The convenience wrapper at line 260 also needs the new parameter:

```rust
pub fn generate_nl_with_call_context(
    chunk: &Chunk,
    ctx: &CallContext,
    callee_doc_freq: &std::collections::HashMap<String, f32>,
    max_callers: usize,
    max_callees: usize,
) -> String {
    generate_nl_with_call_context_and_summary(
        chunk,
        ctx,
        callee_doc_freq,
        max_callers,
        max_callees,
        None,
        None, // no hyde
    )
}
```

- [ ] **Step 3: Fix all callers**

Every call to `generate_nl_with_call_context_and_summary` needs the new `hyde` parameter. Fix each call site:

**`src/cli/enrichment.rs:160`** — add `None` (Task 5 will replace with real value):
```rust
let enriched_nl = cqs::generate_nl_with_call_context_and_summary(
    &chunk,
    &ctx,
    &callee_doc_freq,
    5, // max callers
    5, // max callees
    summary,
    None, // hyde — populated in Task 5
);
```

**`src/nl.rs` test calls** — grep for `generate_nl_with_call_context_and_summary` in tests, add `None` to each call. Also update the `generate_nl_with_call_context` wrapper (line 260) to pass `None`.

- [ ] **Step 4: Run tests**

```bash
cargo test --features gpu-index --lib nl::tests -- --nocapture 2>&1 | tail -5
```

- [ ] **Step 5: Commit**

```
feat(nl): extend NL assembly with hyde query predictions (SQ-12)
```

---

### Task 5: Extend enrichment pass for hyde

**Files:**
- Modify: `src/cli/enrichment.rs`

- [ ] **Step 1: Pre-fetch hyde predictions alongside summaries**

In `enrichment_pass`, after the `all_summaries` pre-fetch (line 90), add a pre-fetch for hyde:

```rust
let all_hyde = match store.get_all_summaries("hyde") {
    Ok(s) => s,
    Err(e) => {
        tracing::warn!(error = %e, "Failed to pre-fetch hyde predictions for enrichment");
        HashMap::new()
    }
};
```

- [ ] **Step 2: Pass hyde to NL generation**

In the chunk loop, after `let summary = ...` (line 123), add the hyde lookup:

```rust
let summary = all_summaries.get(&cs.content_hash).map(|s| s.as_str());
let hyde = all_hyde.get(&cs.content_hash).map(|s| s.as_str()); // NEW
```

Extend the skip condition at line 126 — append `&& hyde.is_none()`:

```rust
if !has_callers && !has_callees && summary.is_none() && hyde.is_none() {
    continue;
}
```

Also extend the ambiguous-name skip condition at line 134 — hyde predictions don't depend on call graph, so ambiguous-named functions with hyde should still be enriched:

```rust
if name_file_count.get(&cs.name).copied().unwrap_or(0) > 1 && summary.is_none() && hyde.is_none() {
    continue;
}
```

Update the NL generation call at line 160 — change `None` to `hyde`:

```rust
let enriched_nl = cqs::generate_nl_with_call_context_and_summary(
    &chunk,
    &ctx,
    &callee_doc_freq,
    5,
    5,
    summary,
    hyde,
);
```

- [ ] **Step 3: Update enrichment hash to include hyde**

In `compute_enrichment_hash_with_summary` (line 209), add hyde text to the hash input:

```rust
fn compute_enrichment_hash_with_summary(
    ctx: &cqs::CallContext,
    callee_doc_freq: &HashMap<String, f32>,
    summary: Option<&str>,
    hyde: Option<&str>,
) -> String {
    use std::fmt::Write;
    let mut input = String::new();

    // ... existing callers/callees/summary logic ...

    if let Some(h) = hyde {
        let _ = write!(input, "h:{h}");
    }

    let hash = blake3::hash(input.as_bytes());
    hash.to_hex()[..32].to_string()
}
```

Update the call site at line 148-149:

```rust
let enrichment_hash =
    compute_enrichment_hash_with_summary(&ctx, &callee_doc_freq, summary, hyde);
```

- [ ] **Step 4: Run tests**

```bash
cargo test --features gpu-index --lib -- --nocapture 2>&1 | tail -5
```

- [ ] **Step 5: Commit**

```
feat(enrichment): incorporate hyde predictions into enrichment pass
```

---

### Task 6: CLI flags and index command wiring

**Files:**
- Modify: `src/cli/commands/index.rs`
- Modify: `src/cli/mod.rs` (if flags are defined on `IndexArgs`)

- [ ] **Step 1: Add `--hyde-queries` and `--max-hyde` flags**

In the index subcommand definition (around `--improve-docs`), add with full attributes:

```rust
/// Generate hyde query predictions for functions (requires ANTHROPIC_API_KEY)
#[cfg(feature = "llm-summaries")]
#[arg(long)]
#[allow(unused_variables)]
hyde_queries: bool,
/// Maximum number of functions to generate hyde predictions for
#[cfg(feature = "llm-summaries")]
#[arg(long)]
#[allow(unused_variables)]
max_hyde: Option<usize>,
```

Update `cmd_index` function signature to accept the new parameters (with `#[allow(unused_variables)]` matching existing pattern):

```rust
pub(crate) fn cmd_index(
    cli: &Cli,
    force: bool,
    dry_run: bool,
    no_ignore: bool,
    #[allow(unused_variables)]
    llm_summaries: bool,
    #[allow(unused_variables)]
    improve_docs: bool,
    #[allow(unused_variables)]
    max_docs: Option<usize>,
    #[allow(unused_variables)]
    hyde_queries: bool,    // NEW
    #[allow(unused_variables)]
    max_hyde: Option<usize>, // NEW
) -> Result<()> {
```

Update the match arm in `src/cli/mod.rs` that destructures `Commands::Index { ... }`. Add cfg-gated extraction matching the existing pattern for `llm_summaries` / `improve_docs`:

```rust
// In the Commands::Index destructure, add:
#[cfg(feature = "llm-summaries")]
hyde_queries,
#[cfg(feature = "llm-summaries")]
max_hyde,

// After the destructure, add the cfg-gated variables:
#[cfg(feature = "llm-summaries")]
let use_hyde = hyde_queries;
#[cfg(not(feature = "llm-summaries"))]
let use_hyde = false;
#[cfg(feature = "llm-summaries")]
let use_max_hyde = max_hyde;
#[cfg(not(feature = "llm-summaries"))]
let use_max_hyde: Option<usize> = None;
```

Then pass `use_hyde` and `use_max_hyde` to `cmd_index`.

- [ ] **Step 2: Wire `hyde_query_pass` into `cmd_index`**

Add between the doc comment pass and the enrichment pass (around line 214):

```rust
#[cfg(feature = "llm-summaries")]
if !check_interrupted() && hyde_queries {
    if !cli.quiet {
        println!("Generating hyde query predictions...");
    }
    let config = cqs::config::Config::load(&root);
    let count = cqs::llm::hyde_query_pass(&store, cli.quiet, &config, max_hyde.unwrap_or(0))
        .context("Hyde query prediction pass failed")?;
    if !cli.quiet && count > 0 {
        println!("  Hyde predictions: {} new", count);
    }
}
```

- [ ] **Step 3: Compile and smoke test**

```bash
cargo build --features gpu-index 2>&1 | tail -5
cargo run --features gpu-index -- index --help 2>&1 | grep hyde
```

- [ ] **Step 4: Commit**

```
feat(cli): --hyde-queries flag for index-time query prediction
```

---

### Task 7: Eval test for hyde predictions

**Files:**
- Modify: `tests/model_eval.rs`

- [ ] **Step 1: Add `test_hyde_predictions` test**

New `#[ignore]` test that:
1. Parses fixture chunks
2. For each chunk, generates mock hyde predictions (simulate what the LLM would produce — use a simple heuristic: tokenized name + first line of doc)
3. Embeds with and without the predictions appended
4. Compares R@1/MRR on hard eval cases

This tests the embedding impact without needing actual API calls. The real LLM predictions will be better than the heuristic, so this is a conservative estimate.

- [ ] **Step 2: Run test**

```bash
cargo test --features gpu-index --test model_eval -- test_hyde_predictions --ignored --nocapture
```

- [ ] **Step 3: Commit**

```
test: hyde prediction embedding impact eval
```

---

### Task 8: Update docs and research log

**Files:**
- Modify: `ROADMAP.md`
- Modify: `CHANGELOG.md`
- Modify: `PROJECT_CONTINUITY.md`

- [ ] **Step 1: Update roadmap**

Mark SQ-12 as in-progress, update the research backlog table.

- [ ] **Step 2: Update research log**

Add Experiment 8 section with hyde prediction results.

- [ ] **Step 3: Commit**

```
docs: SQ-12 hyde query predictions — roadmap and research log
```

---

## Task Parallelism

| Phase | Tasks | Notes |
|-------|-------|-------|
| 1 (parallel) | 1, 2 | Store metadata + LLM prompt/batch (independent files) |
| 2 | 3 | Top-level pass function (depends on 1+2) |
| 3 | 4 | NL assembly signature change (depends on nothing, but blocks 5) |
| 4 | 5 | Enrichment pass extension (depends on 4 — uses new NL signature) |
| 5 | 6 | CLI wiring (depends on 3+5) |
| 6 | 7 | Eval test (depends on 6) |
| 7 | 8 | Docs (depends on 7 results) |
