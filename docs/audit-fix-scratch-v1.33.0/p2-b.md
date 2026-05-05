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
