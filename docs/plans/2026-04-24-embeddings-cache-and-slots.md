# Embeddings Cache + Named Slots

## Summary

Two co-dependent infrastructure pieces:

1. **Content-keyed embeddings cache** at `.cqs/embeddings_cache.db` — `(chunk_hash, model_id) → embedding`. Makes "swap embedder + re-eval" cheap on second and subsequent swaps.
2. **Project-level named slots** under `.cqs/slots/<name>/`. `cqs slot {list,create,promote,remove,active}` subcommand + `--slot` flag / `CQS_SLOT` env on every major command. Enables side-by-side indexes across embedders with clean promote/rollback.

Unlocks the BGE → E5 v9-200k A/B (and future embedder A/Bs) at low cost. The A/B itself is a separate follow-up; this spec ships the infrastructure.

Reference: ROADMAP.md "Embedder swap workflow"; issue #949 (embedder abstraction, closed) was the prior gating blocker.

## Scope

**In:**
- `embeddings_cache` table at `.cqs/embeddings_cache.db`, keyed by `(chunk_hash, model_id)`
- `.cqs/slots/<name>/` per-slot directory containing a full self-contained index (`index.db` + `hnsw_*.bin` + SPLADE artifacts)
- `.cqs/active_slot` pointer file
- `cqs slot {list, create, promote, remove, active}` subcommand
- `cqs cache {stats, prune, compact}` subcommand
- `--slot <name>` flag on every major command; `CQS_SLOT` env fallback
- Cache used transparently during the embed phase
- One-shot migration: existing `.cqs/index.db` + hnsw → `.cqs/slots/default/`

**Out (flag if wanted; defer otherwise):**
- Per-ref embedder config (decided: project-level slots only)
- Global cross-project cache (project-scoped only)
- Cache auto-eviction (manual `cqs cache prune` only)
- Cross-slot query unions (search ONE slot per command)
- Daemon hot-reload on slot promote (restart required)
- UI slot picker in `cqs serve` (accepts `--slot` flag only)
- `cqs index --all-slots` convenience (manual per-slot invocation)
- The BGE → E5 A/B measurement itself (separate follow-up issue)

## Architecture

### Directory layout

After migration:

```
.cqs/
  active_slot                # text file: "default"
  embeddings_cache.db        # cross-slot, content-addressed
  slots/
    default/                 # ex-default (BGE-large today)
      index.db
      hnsw_1024.bin
      splade.bin
    e5/                      # user-created, E5-base preset
      index.db
      hnsw_768.bin
      splade.bin
  watch.sock                 # daemon socket, bound to active slot
```

Each slot is a self-contained full index (its own `chunks` / `function_calls` / `type_edges` / `notes`). Parser runs per slot on a full rebuild. **Cost is acceptable:** parse is <10% of reindex time; cache eliminates the expensive re-embed. Alternative (shared chunks + per-slot embeddings) was considered and rejected — requires dropping `chunks.embedding` column and threading slot_id through every read path. Too much schema surgery for the first cut.

### Migration (schema v23)

On first post-upgrade `cqs` invocation, if `.cqs/index.db` exists AND `.cqs/slots/` does not:

1. Create `.cqs/slots/default/`
2. Move `index.db` + `hnsw_*.bin` + SPLADE artifacts in (atomic rename when possible; file-by-file with rollback inventory when cross-device)
3. Write `.cqs/active_slot = "default"`
4. `embeddings_cache.db` created empty on first index after migration

Idempotent — subsequent runs observe `slots/` and skip. Since cqs has no external users yet, no dual-read backward-compat path — one-shot event.

### Cache

Schema in `.cqs/embeddings_cache.db`:

```sql
CREATE TABLE embeddings_cache (
    chunk_hash BLOB NOT NULL,   -- blake3(content+parser_version), matches chunks.content_hash
    model_id   TEXT NOT NULL,   -- stable: "BAAI/bge-large-en-v1.5@<hf_revision>"
    dim        INTEGER NOT NULL,
    embedding  BLOB NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (chunk_hash, model_id)
) WITHOUT ROWID;

CREATE INDEX idx_cache_model ON embeddings_cache(model_id);  -- for prune-by-model + stats
```

`model_id` format: HF repo + `@` + revision (the HF snapshot hash). Revision captures weight changes; the HF hub client already exposes the resolved snapshot dir. Custom ONNX models use `custom:<path>@<sha256_of_weights>` for stability.

Lookup during embed phase:

```rust
let (hit_idx, miss_idx) = partition(chunks, |c| cache.has(c.content_hash, model_id));
let fresh = embedder.embed_batch(chunks[miss_idx].content);   // one GPU batch, preserves batching efficiency
cache.insert_many(chunks[miss_idx], model_id, fresh);
let all: Vec<Embedding> = hit_idx.cached_embeddings.chain(miss_idx.fresh);
```

Invariants:
- Pre-filter split → one GPU batch on misses only (preserves batching throughput)
- Dim sanity check on read (`cache.dim != model.dim` → treat as stale, re-embed, overwrite)
- `INSERT OR IGNORE` on write (concurrent `cqs index` processes with same hash+model: last writer equivalent to first)

Maintenance commands:

```
cqs cache stats                     # per-model entry counts, total bytes, DB file size
cqs cache prune --model <id>        # drop all entries for a model_id
cqs cache prune --older-than <dur>  # drop entries older than N days (e.g. "30d")
cqs cache compact                   # VACUUM
```

### Slot commands

```
cqs slot list                                     # all slots + active marker + chunk counts + model_id
cqs slot create <name> --model <preset-or-hf>     # mkdir + validate model; index populates
cqs slot promote <name>                           # update active pointer
cqs slot remove <name> [--force]                  # delete slot dir; refuse active unless --force
cqs slot active                                   # print active slot name
```

Slot name: `[a-z0-9_-]+`, max 32 chars. Reserved words rejected: `default` (pre-reserved for migration), `active`, `list`, `create`, `promote`, `remove`. `--model` routes through existing `ModelConfig::resolve` so presets (`bge-large`, `e5-base`, `v9-200k`) and HF IDs both work.

`cqs slot remove <active>` with no other slot present: refused even with `--force` (can't leave the project with no active slot).

### Slot resolution order

Every major command (`index`, `search`, `scout`, `gather`, `impact`, `context`, `watch`, `serve`, `brief`, `blame`, `callers`, `callees`, `test-map`, `review`, `where`, `similar`, `notes`, `batch`, …):

```
1. --slot <name>            (explicit flag)
2. $CQS_SLOT                (env)
3. .cqs/active_slot file    (project default)
4. "default"                (fallback)
```

Missing slot dir at resolution: actionable error:

```
Slot '<name>' not found. Available: [default, e5]. Create with:
    cqs slot create <name> --model <model-id>
```

### Daemon behaviour

- Daemon binds to whichever slot is active **at startup**. Active slot value captured in daemon state at socket open.
- Promoting a different slot: daemon keeps serving the old one. `cqs slot promote` emits:

```
Active slot changed to '<name>'. To serve queries from the new slot, restart
the daemon:
    systemctl --user restart cqs-watch
```

- Why not hot-reload: daemon holds HNSW in memory for fast queries; live swap is nontrivial (dim change invalidates warm CUDA state, in-flight queries would need to be cancelled or migrated). Out of scope for first cut. Filed as follow-up after shipping.

## Configuration

| Env var                      | Scope                | Default  | Purpose                                                                                 |
|------------------------------|----------------------|----------|-----------------------------------------------------------------------------------------|
| `CQS_SLOT`                   | per-invocation       | (unset)  | Select slot for this command. Overridden by `--slot` flag, overrides `.cqs/active_slot` |
| `CQS_CACHE_ENABLED`          | per-invocation       | `1`      | Set `0` to disable cache entirely for this run (for benchmarking / debugging)           |
| `CQS_CACHE_MAX_BYTES`        | per-invocation       | (unset)  | Soft cap; emit `tracing::warn!` when cache DB exceeds; no auto-prune                    |

No new config required for basic use. Slots promote the existing `--model` / `CQS_EMBEDDING_MODEL` plumbing.

## Error handling

| Scenario                                                 | Response                                                                                             |
|----------------------------------------------------------|------------------------------------------------------------------------------------------------------|
| Cache DB corruption                                      | `tracing::warn!`, disable cache for this run, re-embed everything. Don't block indexing.             |
| Slot in `active_slot` doesn't exist on disk              | Actionable error: suggest `cqs slot list`                                                            |
| Slot dir present but `index.db` missing                  | "Slot exists but never indexed. Run `cqs index --slot <name>` first."                                |
| Concurrent `cqs index --slot <name>` on same slot        | Existing SQLite advisory locking applies (unchanged)                                                 |
| `cache.dim != model.dim` on lookup                       | Stale entry, re-embed, overwrite with `tracing::warn!`                                               |
| Migration failure mid-move                               | Rollback using inventory list (atomic rename path does not need rollback); emit detailed failure    |
| Concurrent `cqs slot promote` from two shells            | Atomic rename on pointer file: last writer wins; subsequent resolution sees the newer value          |
| `cqs slot remove default` when default is the sole slot  | Refused even with `--force` — "at least one slot must exist"                                         |
| Corrupt `.cqs/active_slot`                               | Fall back to "default" with `tracing::warn!`; emit actionable hint to `cqs slot active` / `promote`  |
| Disk full during migration                               | Rollback any moved files using inventory; report remaining source state; don't partial-commit       |

## Tracing

```rust
// Outer index command
tracing::info_span!("index_slot", slot_name, model_id, dim).entered();

// Cache phase
tracing::info!(hits, misses, hit_rate = format!("{:.1}%", ...), "embeddings cache status");
tracing::warn!(model_id, stale_dim, current_dim, "cache dim mismatch, re-embedding");
tracing::debug!(cache_db = %path, "cache opened");
tracing::warn!(err = %e, "cache DB corrupt; disabling cache for this run");

// Slot resolution
tracing::info!(slot_name, source = "flag|env|file|fallback", "active slot resolved");

// Migration
tracing::info!(from, to, files_moved, "legacy index.db migrated to slots/default/");
tracing::error!(err = %e, "migration failed; rollback in progress");

// Daemon
tracing::info!(slot_name = %active_at_startup, "daemon bound to slot");
tracing::warn!(new_slot, "active slot changed; daemon still serving old slot — restart required");
```

Smoke verification: `RUST_LOG=cqs=debug cqs index --slot e5` surfaces the span tree with hit/miss counts per embed batch.

## Testing

### Cache unit tests (`src/cache/embeddings.rs::tests`, ~10 tests)

1. Empty cache: miss → embed → insert → second lookup hits with same bytes
2. Dim mismatch on read: re-embed + overwrite
3. Concurrent `INSERT OR IGNORE` on same key: both succeed, last write preserved
4. Prune by model_id: only that model's entries removed
5. Prune by age: only older-than-N removed
6. `VACUUM` reduces file size after prune
7. Corrupt DB: open fails → fall back + warn path exercised
8. Model ID round-trip preserves HF revision suffix exactly
9. Custom ONNX model_id (`custom:<path>@<sha256>`) accepted end-to-end
10. Cache stats: per-model counts match inserted rows

### Slot unit tests (`src/slot/mod.rs::tests`, ~12 tests)

1. Name validation: alphanumeric + `_-` accepted; uppercase / spaces / reserved words rejected
2. Reserved word rejection: `default`, `active`, subcommand names
3. Create → list → promote → remove lifecycle
4. Remove active without `--force`: rejected
5. Remove active with `--force` when other slot exists: active pointer updated to other slot
6. Remove the last remaining slot: refused even with `--force`
7. Migration: `.cqs/index.db` + hnsw → `slots/default/`, idempotent on re-run
8. Resolution order: flag > env > file > default (table-driven test)
9. Corrupted `active_slot` file (non-UTF8 bytes) → fall back to default with warning
10. Slot name max-length enforcement (32 chars)
11. Promote non-existent slot: actionable error
12. Create slot with unknown `--model`: actionable error at CLI parse

### Integration tests (`tests/slot_integration.rs`, ~8 tests)

13. Full index → create second slot → index second slot → assert cache hits for common chunks
14. Promote second slot → query returns second-slot results (different vectors → different top-K)
15. Incremental reindex after 1-file edit: cache hit rate ≥95% assertion
16. Two slots with different dims (BGE 1024, E5 768): both queryable via `--slot`; HNSW sizes differ as expected
17. `cqs slot promote` emits the daemon-restart warning line on stdout
18. Cache across `cqs index --slot a` + `cqs index --slot b`: disk size bounded by unique (hash, model) pairs
19. `cqs cache stats` reflects entries after indexing
20. `cqs cache prune --model <removed>` leaves other models' entries intact

### Sad paths (part of above, ~8 cases)

21. Missing slot dir referenced by active_slot → actionable error
22. Slot dir with no `index.db` → actionable error
23. Slot name collision with subcommand → rejected at clap parse stage
24. `cqs slot remove default --force` when default is active sole slot → clear error
25. Simulated disk full during migration → rollback + error, source dir restored
26. Cache DB corruption → warn + continue, batch completes
27. Two parallel `cqs slot promote` invocations → last-writer-wins, resolution is consistent after both return
28. Cache miss when model preset is renamed (`e5-base` → `e5`): handled as new `model_id`, both entries coexist (model_id captures the exact identity)

## Rollout

Single PR. Cache + slots + migration are co-dependent per the roadmap note ("Build [slots] only after the cache lands").

Acceptance criteria:

- [ ] All existing 1679 lib tests pass; no regression in daemon / index / search paths
- [ ] ~38 new tests (§Testing), all pass
- [ ] Migration runs cleanly on real cqs-on-cqs project: existing `.cqs/index.db` lands in `.cqs/slots/default/`; active slot resolves correctly; queries return identical results pre/post migration
- [ ] End-to-end demo (manual acceptance, documented in PR):
      ```
      cqs slot create e5 --model e5-base
      cqs index --slot e5
      cqs slot promote e5
      cqs search "some query"   # queries E5 slot
      cqs slot promote default
      cqs search "same query"   # queries BGE slot
      ```
- [ ] Cache hit rate ≥95% on cqs-on-cqs after a single-file edit (measurement logged in PR description)
- [ ] README updated: `cqs slot` subcommand docs + `CQS_SLOT` / `CQS_CACHE_*` env var rows
- [ ] `cqs doctor` checks `active_slot` file integrity + slot dir presence
- [ ] No new clippy warnings under `--features gpu-index`
- [ ] Tracing spans verified present via `RUST_LOG=cqs=debug` smoke run
- [ ] ROADMAP.md updated: cache + slots checked off; BGE → E5 A/B item notes it is now unblocked

## Known limitations

- **Parser runs per-slot on full rebuild.** Cost: parse time × slot count. Acceptable — parse is <10% of a reindex; cache eliminates the expensive re-embed.
- **Daemon doesn't auto-reload on slot promote.** Manual `systemctl --user restart cqs-watch` required. Filed as follow-up issue.
- **Cache is project-scoped.** Same source file in two projects re-embeds in each. Acceptable — chunks with identical content across projects is rare in practice.
- **No `cqs index --all-slots`.** User runs `cqs index --slot <each>` manually. Add later if it bites.
- **Slot promotion invalidates daemon HNSW warmth.** Pre-existing concern around HNSW warm-state; not made worse by this change, but flagged so the follow-up reload work is aware.
- **BGE → E5 A/B itself is not part of this PR.** That measurement is a separate concern that uses this infrastructure.

## Follow-up issues to file

- `feat: daemon hot-reload on slot promote` (respects dim change + in-flight query drain)
- `feat: cqs index --all-slots` (iterate all slots, populate cache once, respect per-slot state)
- `bench: BGE vs E5 v9-200k on v3.v2 under paired-reindex protocol` (the actual A/B; uses this infra)
