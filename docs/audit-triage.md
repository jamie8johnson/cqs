# Audit Triage — v1.22.0

Triage date: 2026-04-11. Source: `docs/audit-findings.md` (136 findings across 16 categories, two batches of 8 parallel auditors).

## Triage rules (why things land where they do)

1. **Regressions are P1 unless a documented reason says otherwise.** User rule, 2026-04-11. A regression that narrowly affects one user path is still P1.
2. **Documents that make factual claims contradicted by the code are P1.** SECURITY.md/PRIVACY.md saying the program does X when it does Y is a correctness bug, not a doc chore.
3. **PR #895 tax.** Cross-auditor cluster: CQ-2, CQ-3, DS-W1/W2/W3/W4, EH-1/2/3/4, PB-NEW-1/2/3/4/5, PF-4, RB-1/2, RM-3/4/5, and adversarial test gaps all trace to the SPLADE persistence PR shipped earlier today. Every one of them is P1 unless explicitly deferred, because the whole cluster makes the shipped PR unsound in normal operation.
4. **Watch+SPLADE drift** is triple-confirmed (OB-22, PB-NEW-6, DS-W2) and the failure mode is silent stale data. Auto-P1.
5. **Batch mode SPLADE invalidation** is quintuple-confirmed (CQ-2, EH-8, RM-3, happy-path TC-2, the existing `test_invalidate_clears_mutable_caches` test would fail if extended). Auto-P1, trivial fix.
6. **AC-1 — SPLADE hybrid fusion scores discarded** is hard but correctness-critical. Every SPLADE eval we have is measuring the wrong thing until this is fixed. P1 despite the hard difficulty, with the caveat that the fix reshapes evaluation methodology.
7. **Flag-declared-but-unread cluster**: CQ-1, API-1, API-10, API-11 are four independent findings pointing at the same root — cqs has no test asserting that every declared clap flag has at least one consumer. Fix the individual drops as P1; add a meta-test to prevent recurrence as P2.
8. Hard security/data-safety items that predate #895 (DS-W5 watch-rebuild race, DS-W6 schema-migration race, SEC-NEW-1 reference source exfil) are P1 by severity, not by regression rule.

## Summary

| Tier | Count | Criterion |
|---|---|---|
| **P1** | ~50 | Regression, data-safety correctness, or security boundary. Fix in this audit cycle. |
| **P2** | ~30 | Medium effort with meaningful impact, or easy with medium impact. Fix if time before next release. |
| **P3** | ~40 | Easy but lower-impact hygiene. Fix inline with related work or batch into one cleanup PR. |
| **P4** | ~15 | Hard, architectural, or genuinely low-impact. Create issues; do not block this cycle. |

---

## P1 — Fix this cycle

### Data safety / PR #895 correctness cluster (regression tier)

| ID | Finding (short) | File | Why P1 |
|---|---|---|---|
| CQ-2 / RM-3 / EH-8 / TC-2 | `BatchContext::invalidate_mutable_caches` omits `splade_index` → stale batch sessions serve stale SPLADE forever | `src/cli/batch/mod.rs:178-186` | Quintuple-confirmed. Easy one-line fix. Regression. | Status: ⏳ |
| CQ-3 | `prune_orphan_sparse_vectors` DELETE + SELECT + UPDATE non-transactional | `src/store/sparse.rs:229-262` | Regression from #895 adding untransactioned queries. Wrap in `begin_write`. | Status: ⏳ |
| DS-W1 | `prune_missing` / `prune_all` delete sparse rows in-tx but never bump `splade_generation` | `src/store/chunks/staleness.rs:116-128, 248-260` | Extends the #895 generation-counter hole; stale on-disk indexes trusted after GC. | Status: ⏳ |
| DS-W2 / OB-22 / PB-NEW-6 | `cqs watch` has zero SPLADE integration; sparse vectors go stale silently; `splade_generation` never bumped | `src/cli/watch.rs:684-882` | **Triple-confirmed**. Loudest finding in the audit. Two-part fix: (a) bump generation in watch writes to force rebuild-on-next-query; (b) wire SPLADE encoder into watch for full re-encode. | Status: ⏳ |
| DS-W3 | `sparse_vectors` has no FK with ON DELETE CASCADE → three delete paths in `chunks/crud.rs` leak orphan sparse rows | `src/store/chunks/crud.rs:414-588` + `schema.sql` | Root cause of DS-W1. v19 migration adding FK + CASCADE is the structural fix that makes forgetting impossible. | Status: ⏳ |
| DS-W4 | TOCTOU: `splade_generation()` read separately from row-data read in three load sites | `src/cli/commands/index/build.rs:512-562`, `src/cli/store.rs:172-210`, `src/cli/batch/mod.rs:281-320` | Mine. Persist can label gen-N data as gen-M silently. Fix: single read transaction. | Status: ⏳ |
| EH-1 / OB-17 | `splade_generation()` silent parse-to-0 on corrupt metadata; same pattern in two bump sites | `src/store/sparse.rs:270-278`, `:130-137`, `:244-251` | Collapses the invalidation counter on corruption; pairs with EH-3 into self-perpetuating cache poison. | Status: ⏳ |
| EH-2 | `cmd_index` persists SPLADE with bare `.unwrap_or(0)` on generation read | `src/cli/commands/index/build.rs:536` | Violates project rule ("never bare `.unwrap_or_default()`"). Mine, shipped today. | Status: ⏳ |
| EH-3 | `load_or_build` callers substitute `0` on generation-read failure and then `save()` a gen-0 file → self-perpetuating cache-poison loop | `src/cli/store.rs:172-210`, `src/cli/batch/mod.rs:281-320` | The failure mode is silent forever-rebuild. Fix: return `None` from the caller instead of falling through with `0`. | Status: ⏳ |
| EH-4 | `SpladeIndexPersistError::Io` overloaded for 5 non-IO corrupt-data conditions | `src/splade/index.rs:227-260, 405-472` | API regret, shipped today. Add `CorruptData(String)` variant. | Status: ⏳ |
| RB-1 | SPLADE header `chunk_count` / `token_count` not in blake3 checksum → one-bit flip → `Vec::with_capacity(usize::MAX)` panic | `src/splade/index.rs:389-411` | OOM attack vector. HNSW already defends against this class. | Status: ⏳ |
| RB-2 | `SpladeIndex::load()` has no file-size cap before `read_to_end` | `src/splade/index.rs:395-396` | Unbounded allocation from untrusted file. HNSW has `hnsw_max_graph_bytes()` / `hnsw_max_data_bytes()`. | Status: ⏳ |
| PB-NEW-1 | SPLADE save has no file locking; HNSW has `hnsw.lock` | `src/splade/index.rs:208-333` | Concurrent save tears the file. | Status: ⏳ |
| PB-NEW-2 | SPLADE save emits no WSL advisory-locking warning | `src/splade/index.rs:208` | Silent on WSL, unlike HNSW. | Status: ⏳ |
| PB-NEW-3 | Windows `remove_file + rename` is **redundant and actively harmful** — `std::fs::rename` handles replace-existing via `MoveFileExW` since Rust 1.46 | `src/splade/index.rs:319-325` | Delete the `#[cfg(windows)]` block. Introduces a TOCTOU race solving a non-problem. | Status: ⏳ |
| PB-NEW-4 | SPLADE save has no cross-device `fs::copy → rename` fallback; HNSW has one | `src/splade/index.rs:325` | WSL 9P / Docker overlayfs breaks raw rename. | Status: ⏳ |
| PF-4 | SPLADE body `Vec::new()` has no capacity hint → ~log₂(59MB) reallocations, ~100ms memcpy | `src/splade/index.rs:395-396` | One-line fix: pre-allocate from file metadata. Mine, #895 paper cut. | Status: ⏳ |
| RM-4 | `SpladeIndex::save` leaks orphan `.splade.index.bin.*.tmp` on crash; HNSW has a cleanup loop | `src/splade/index.rs:284-325` | Add cleanup at the top of `load()`, mirror HNSW pattern. | Status: ⏳ |

### Scaling / performance regressions (wiring not verified after a fix)

| ID | Finding (short) | File | Why P1 |
|---|---|---|---|
| SHL-31 | `upsert_sparse_vectors` DELETE loop still uses `chunks(333)` after PR #891 fixed only the INSERT | `src/store/sparse.rs:51-62` | Missed sibling in PR #891. Same perf class. Easy fix. | Status: ⏳ |
| SHL-32 | `CHUNK_INSERT_BATCH = 49` in the primary chunk indexing path is 30× too small for modern SQLite | `src/store/chunks/async_helpers.rs:220-242` | Hottest reindex path; same root as SHL-31 but bigger blast radius. | Status: ⏳ |
| SHL-33 | 14 other store call sites carry explicit "999" constants or comments | `src/store/*` | Mechanical cleanup driven by one shared helper. | Status: ⏳ |

### Algorithm correctness (regression even if not recent)

| ID | Finding (short) | File | Why P1 |
|---|---|---|---|
| AC-1 | **SPLADE hybrid fusion scores are discarded.** `search_hybrid` computes alpha-weighted score, sorts, then passes IDs to a function that re-scores cosine-only. The alpha knob is a no-op on final ranking. | `src/search/query.rs:502-540, 700-734` | Every SPLADE eval on record is measuring candidate-set expansion, not fusion. Today's +10pp cross_language and -0.6pp overall are both reinterpretations under this finding. Hard, but blocks trustworthy SPLADE evaluation. | Status: ⏳ |
| AC-2 | Router negation classifier matches `"not "` inside `cannot`, `"no "` inside `piano`/`nano`/`volcano`/`casino` | `src/search/router.rs:193-204, 288` | Common code identifiers misroute to `DenseBase` silently. Easy — switch to word-boundary tokenization. | Status: ⏳ |
| AC-4 | `is_test_chunk` demotes production types `TestRegistry`/`TestHarness`/`TestRunner`/`TestContext` 30% via `starts_with("Test")` | `src/lib.rs:241` | Search rank corruption on common DI-container types. Easy. | Status: ⏳ |

### Security / privacy (high severity regardless of age)

| ID | Finding (short) | File | Why P1 |
|---|---|---|---|
| SEC-NEW-1 | Reference `source` field unvalidated → arbitrary file read via checked-in `.cqs.toml`. Attacker ships `source = "/home/user/.ssh"`; victim runs `cqs ref update rust-std-docs`; SSH keys get indexed into a reference DB | `src/config.rs:293-309`, `src/cli/commands/infra/reference.rs:110-111, 303-380` | Data exfiltration via a commonly-committed file. Pre-existing (not from #895) but clearly P1. | Status: ⏳ |
| CQ-5 | `HnswIndex::try_load_with_ef(None, None)` in `reference.rs:106` and `project.rs:322` passes default dim (1024) even when `store.dim()` is available → 768-dim references silently load as 1024-dim garbage | `src/reference.rs:106`, `src/project.rs:322` | Same class as the `build_batched()` disaster from PR #690. Cross-project search returns garbage silently. Easy. | Status: ⏳ |

### Documentation that lies

| ID | Finding (short) | File | Why P1 |
|---|---|---|---|
| Doc-2 | **SECURITY.md:22 claims `integrity_check(1) on every database open`** — false since #893 | `SECURITY.md:22` | Security doc contradicts current code. The doc lies about safety behaviour. | Status: ⏳ |
| Doc-3 | SECURITY.md:71/84 read/write access tables omit `.cqs/splade.index.bin` | `SECURITY.md:71, 84` | Under-reports filesystem access. Easy. | Status: ⏳ |
| Doc-11 | PRIVACY.md:7 says `CQS_TELEMETRY=1` gates telemetry; an existing `telemetry.jsonl` re-activates logging without the env var | `PRIVACY.md:7` | Privacy doc lies about opt-in semantics. Easy. | Status: ⏳ |
| Doc-1 | `CHANGELOG.md [Unreleased]` empty despite four shipped session PRs | `CHANGELOG.md:8-9` | Release hygiene. Easy. | Status: ⏳ |

### Flag-declared-but-unread cluster (four independent findings, same root)

| ID | Finding (short) | File | Why P1 |
|---|---|---|---|
| CQ-1 | `set_rrf_k_from_config` dead — config value silently ignored | `src/store/search.rs:13-19`, `src/store/mod.rs:162` | User writes `[scoring] rrf_k = 40`, nothing happens. | Status: ⏳ |
| API-1 | `--format` flag ignored on 25+ commands (only 4 use `effective_format()`) | `src/cli/dispatch.rs:172-315` | User runs `cqs stats --format json` → text output. | Status: ⏳ |
| API-10 | `--semantic-only` flag defined on `Cli` but zero readers | `src/cli/definitions.rs:181-183` | Dead flag advertised in help. | Status: ⏳ |
| API-11 | `cmd_plan --tokens` respected in JSON mode, silently dropped in text mode | `src/cli/commands/train/plan.rs:37-74` | Budget contract broken on one output mode. | Status: ⏳ |

### Non-transactional / data-loss pre-existing bugs

| ID | Finding (short) | File | Why P1 |
|---|---|---|---|
| DS-W5 | `cqs index --force` renames `index.db` out from under a running `cqs watch`; watch's writes vanish silently as the orphan inode is garbage-collected | `src/cli/commands/index/build.rs:116-167, 609-612` | Data loss during the recovery path. No inter-process lock. Medium effort, high severity. | Status: ⏳ |
| DS-W6 | Concurrent `check_schema_version` races produce spurious "duplicate column" migration failures on healthy DBs | `src/store/metadata.rs:23-74`, `src/store/migrations.rs:29-57, 286-295` | Not corruption but crash-looking failure. Easy fix: re-read version inside migration transaction. | Status: ⏳ |

---

## P2 — Fix if time before next release

### Performance + architecture

| ID | Finding (short) | File |
|---|---|---|
| CQ-4 | `cmd_index` unconditionally re-encodes all SPLADE on every run → silently negates #895 persist win | `src/cli/commands/index/build.rs:389-568` + `src/store/sparse.rs:127-144` |
| PF-1 | No persistent daemon — every CLI invocation pays full startup tax (tokio + ONNX + HNSW + SPLADE load). Agents burst 5-20 queries per turn | `src/main.rs:14-32`, `src/cli/dispatch.rs:23` |
| PF-2 | `EmbeddingCache` only populated during indexing; `embed_query` uses per-process LRU → repeated queries re-run ONNX | `src/embedder/mod.rs:536-592`, `src/cache.rs:145-200` |
| PF-3 | CAGRA rebuilt from store every CLI call (~95MB pull) at 24k vectors. Comment says "1s for 474 vectors" — stale | `src/cli/store.rs:253`, `src/cagra.rs:432-490` |
| RM-6 | `cqs watch` re-opens `Store::open` every reindex cycle; churns 4-thread runtime over 24/7 systemd lifetime | `src/cli/watch.rs:384-387`, `src/store/mod.rs:338-342` |
| RM-7 | `cqs batch`/`cqs chat` open primary store read-write for a single-stdin consumer → 4-thread runtime + quick_check per session | `src/cli/batch/mod.rs:578, 154, 195` |
| SHL-37 | `Embedder::embed_documents` ignores `CQS_EMBED_BATCH_SIZE` — same bug as SHL-27 in a second location | `src/embedder/mod.rs:507-521` |
| PF-11 | SPLADE `load()` copies 59MB into heap instead of `Mmap` | `src/splade/index.rs:341-490` |
| PB-NEW-10 | `load_all_sparse_vectors` holds 7.58M SqliteRows in RAM during SPLADE rebuild → OOM risk on WSL small VMs | `src/store/sparse.rs:158-200` |
| RM-5 | `SpladeIndex::save` holds body Vec + postings HashMap simultaneously → ~2× memory during persist | `src/splade/index.rs:218-262` |
| RM-1 | `Embedder::model_fingerprint` reads 1.3 GB ONNX into heap; HNSW already streams via `update_reader` | `src/embedder/mod.rs:341-363` |

### Extensibility / structural

| ID | Finding (short) | File |
|---|---|---|
| EXT-7 | `LANGUAGE_NAMES` hardcodes 21 of 54 languages next to an existing registry `all()` API | `src/search/router.rs:221-243` |
| EXT-8 | `extract_type_hints` hardcodes 17 patterns, misses 13+ ChunkType variants | `src/search/router.rs:515-549` |
| EXT-9 | `is_test_chunk` duplicates `REGISTRY.all_test_path_patterns()` — 52 languages' test patterns invisible to scoring demotion + dead-code filter | `src/lib.rs:238-269` |
| EXT-11 | `SearchStrategy::DenseWithSplade` has zero callers; `cmd_query_project` dispatch uses boolean instead of match | `src/search/router.rs:71-85`, `src/cli/commands/search/query.rs:279-303` |
| EXT-10 | `parse_source` hardcodes per-language custom parser dispatch; no `LanguageDef.custom_parser` seam | `src/parser/mod.rs:232-244, 205-211` |
| API-5 | Batch `search` silently drops `--context`/`--expand`/`--no-stale-check` and is missing `--threshold`/`--pattern`/`--include-docs`/`--semantic-only` | `src/cli/batch/handlers/search.rs:56-57`, `src/cli/batch/commands.rs:82-92` |

### Observability

| ID | Finding (short) | File |
|---|---|---|
| OB-14 | `cli::run_with` top-level dispatch has no root tracing span — all per-command logs orphaned | `src/cli/dispatch.rs:23` |
| OB-15 | PR #893's `quick_check` + `CQS_SKIP_INTEGRITY_CHECK` bypass paths are silent | `src/store/mod.rs:430-441` |
| OB-16 | `telemetry::log_routed` silently swallows write failures (unlike `log_command`) + no advisory lock | `src/cli/telemetry.rs:106-141` |
| OB-18 | `cagra.rs` 14 format-string log calls vs. structured fields | `src/cagra.rs:93..507` |
| OB-19 | Corrupt embedding skips logged at `trace!` level (invisible at default) — inconsistent with sibling `warn!` | `src/store/chunks/embeddings.rs:52`, `query.rs:347` |
| OB-20 | `Store::begin_write` no span — WRITE_LOCK contention invisible | `src/store/mod.rs:506-518` |

### Error handling (pre-existing silent fallbacks)

| ID | Finding (short) | File |
|---|---|---|
| EH-5 | `parse_server_code_calls/types` silently return empty on 5 parser failure points; sibling `parse_server_code` logs all | `src/parser/aspx.rs:303-355, 425-478` |
| EH-6 | `EmbeddingBatchIterator::next` silently drops rows with corrupt embedding blobs via `.ok()` | `src/store/chunks/async_helpers.rs:438-441` |
| EH-7 | Watch-mode HNSW load failure indistinguishable from "first run" — hides `DimensionMismatch` / IO errors | `src/cli/watch.rs:307-314` |
| EH-9 | `Drop for Store` discards `catch_unwind` panic payload silently | `src/store/mod.rs:621-629` |

### Scaling (env vars to add)

| ID | Finding (short) | File |
|---|---|---|
| SHL-34 | `busy_timeout(5s)` / `idle_timeout(30s)` hardcoded, no env override | `src/store/mod.rs:350, 372` |
| SHL-35 | `max_connections = 4` on write opens, no env override | `src/store/mod.rs:281` |
| SHL-36 | `mmap_size = 256MB` hardcoded in three places | `src/store/mod.rs:282, 304, 320` |
| SHL-38 | SPLADE 4000-char truncation duplicated in two places, no env override | `src/splade/mod.rs:368-382, 533-548` |
| SHL-39 | `MAX_QUERY_BYTES = 32KB` hardcoded, no override | `src/embedder/mod.rs:534` |
| SHL-40 | `HNSW_BATCH_SIZE = 10_000` duplicated in two builders, no override | `src/cli/commands/index/build.rs:680, 716` |
| Roadmap CPU | `PRAGMA quick_check` is 40s on WSL `/mnt/c` on write opens; make `CQS_INTEGRITY_CHECK=1` opt-in instead of opt-out | `src/store/mod.rs:430-441` |

### Meta-fixes (to prevent clusters recurring)

| ID | Finding | Why P2 |
|---|---|---|
| META-1 | Add a test that asserts every declared clap flag has at least one runtime reader (catches CQ-1/API-1/API-10/API-11 class) | Root cause of the flag-drop cluster; one compile-time or one test-time check eliminates four independent P1 findings at a stroke. |
| META-2 | Replace "instrumented invalidation counters" with schema triggers / `ON DELETE CASCADE` (see feedback memory file) | Root cause of the DS-W1/W2/W3/W4 cluster. Currently enforced at call sites; should be enforced by the schema. |

---

## P3 — Fix inline with related work or batch cleanup PR

### API hygiene (easy)

| ID | Finding (short) | File |
|---|---|---|
| CQ-6 | `CommandContext::splade_index` / `BatchContext::ensure_splade_index` logic duplicated | `src/cli/store.rs:144-210` + `src/cli/batch/mod.rs:247-320` |
| CQ-7 | `make_named_store` test helper duplicate — marked "fixing" in v1.20.0, still there | `src/store/calls/cross_project.rs:278`, `src/impact/cross_project.rs:291` |
| API-2 | Subcommand enums (`CacheCommand`, `NotesCommand::List`, `ProjectCommand::Search`, `RefCommand::List`) use inline `json: bool` instead of `TextJsonArgs` | multiple |
| API-3 | `--expand` has two incompatible meanings (bool for parent context vs `usize` for graph depth) | `src/cli/definitions.rs:219`, `src/cli/args.rs:14-16` |
| API-4 | `blame --depth`/`-d` collides with graph-depth flags; AD-31 fix renamed `-n` → `-d` and re-introduced the collision | `src/cli/args.rs:105-114` |
| API-6 | `Affected` missing `--stdin` while peers `ImpactDiff`/`Review`/`Ci` have it | `src/cli/definitions.rs:320-327` |
| API-7 | Batch `ensure_splade_index`+`borrow_splade_index` two-phase API vs CLI one-call | `src/cli/batch/mod.rs:281-327` |
| API-8 | `SpladeIndex::ChecksumMismatch` missing `{ file, expected, actual }` vs HNSW equivalent | `src/splade/index.rs:55-56` |
| API-9 | `SpladeIndexPersistError` `Io` variant overloaded (covered by EH-4, file-level fix) | `src/splade/index.rs:42-59` |
| API-12 | `ProjectCommand::Search --limit` default 10 vs top-level default 5 | `src/cli/commands/infra/project.rs:52-57` |
| API-13 | `TrainData::max_commits = 0` sentinel vs `TrainPairs::limit: Option<usize>` — two neighbouring commands, opposite "unlimited" idioms | `src/cli/definitions.rs:725-743, 745-759` |
| API-14 | `bump_splade_generation` 12-line block duplicated across upsert + prune | `src/store/sparse.rs:126-144, 243-258` |

### Documentation drift (easy)

| ID | Finding (short) | File |
|---|---|---|
| Doc-4 | README.md:35 schema version two versions behind (claims v16, actual v18) | `README.md:35` |
| Doc-5 | README env-var table missing 8 `CQS_*` vars | `README.md:646-690` |
| Doc-6 | README `CQS_WATCH_MAX_PENDING` default wrong (1000 vs actual 10000) | `README.md:689` |
| Doc-7 | CONTRIBUTING.md Architecture Overview missing `src/splade/` | `CONTRIBUTING.md:117-283` |
| Doc-8 | CONTRIBUTING.md missing `src/search/router.rs` | `CONTRIBUTING.md:189-194` |
| Doc-9 | CONTRIBUTING.md missing `src/store/sparse.rs` | `CONTRIBUTING.md:161-173` |
| Doc-10 | `src/store/search.rs:51,55` stale "(in search.rs)" doc comment (moved to `src/search/query.rs` in v0.9.0) | `src/store/search.rs:51, 55` |

### Extensibility / hygiene (easy)

| ID | Finding (short) | File |
|---|---|---|
| EXT-12 | No `INDEX_DB_FILENAME` constant; `"index.db"` literal in 40+ sites | multiple |
| EXT-13 | `ModelConfig::from_preset` has no enumerated preset list | `src/embedder/models.rs:94-101` |

### Hot-loop perf nits (easy, small wins)

| ID | Finding (short) | File |
|---|---|---|
| PF-5 | SPLADE load allocates 24k Strings via `.to_string()` per row | `src/splade/index.rs:411, 434` |
| PF-6 | `name.to_lowercase()` per candidate in `NameMatcher::score` hot loop | `src/search/scoring/name_match.rs:94, 121-124` |
| PF-7 | `search_by_candidate_ids` allocates lowercased strings per row despite pre-lowercased sets — use `eq_ignore_ascii_case` | `src/search/query.rs:691-717` |
| PF-8 | `finalize_results` rebuilds glob matcher already compiled upstream | `src/search/query.rs:306-317` |
| PF-9 | `apply_parent_boost` clones `parent_type_name` per entry in HashMap::entry | `src/search/scoring/candidate.rs:59-63` |
| PF-10 | Store + cache build separate tokio runtimes per invocation (~10-20ms each) | `src/store/mod.rs:333-342`, `src/cache.rs:67-70` |

### Security hardening (defence in depth)

| ID | Finding (short) | File |
|---|---|---|
| SEC-NEW-2 | `log_routed` missing umask/advisory-lock symmetry with `log_command` | `src/cli/telemetry.rs:106-141` |
| SEC-NEW-3 | `run_git_log_line_range` doesn't reject absolute / `..` paths | `src/cli/commands/io/blame.rs:69-111` |

### Platform nits

| ID | Finding (short) | File |
|---|---|---|
| PB-NEW-5 | No parent-directory `fsync` after rename — power-cut can lose SPLADE save (rebuildable so low severity, but undocumented) | `src/splade/index.rs:314-325` |
| PB-NEW-7 | SPLADE save builds 60-100MB body in memory — blocks watch loop on WSL 9P | `src/splade/index.rs:218-315` |
| PB-NEW-9 | `file_name().to_str().unwrap_or("splade.index")` collapses non-UTF-8 paths to a shared temp name | `src/splade/index.rs:284-291` |

### Observability nits

| ID | Finding (short) | File |
|---|---|---|
| OB-13 | `search_hybrid` silently falls back when `splade_index` is empty | `src/search/query.rs:407-409` |
| OB-21 | `BoundedScoreHeap::push` non-finite warn has no context fields | `src/search/scoring/candidate.rs:172` |

### Test additions (happy-path gaps)

All of `docs/audit-findings.md` § Test Coverage (Happy Path) go to P3 unless referenced above. The `test_invalidate_clears_mutable_caches` extension that catches CQ-2 moves to P1 (bundled with the CQ-2 fix).

### Other low-impact

| ID | Finding (short) |
|---|---|
| AC-3 | `bootstrap_ci(&values, 0)` integer underflow — test-only |
| SHL-41 | `SAFETY_MARGIN_VARS = 300` comment is arithmetically wrong (works in practice, but mine and the comment is misleading) |

---

## P4 — Hard or genuinely low-impact (create issues, do not block this cycle)

| ID | Finding (short) | Status |
|---|---|---|
| PF-1 (hard) | Persistent daemon / `cqs serve` — biggest strategic perf win | Create issue |
| PF-3 (hard) | Persist CAGRA graph to disk | Create issue |
| AC-1 fix implementation (hard) | `search_hybrid` rewrite to preserve fused scores through finalize; re-run SPLADE evals | P1 severity but hard implementation — track as issue + roadmap item |
| DS-W5 fix implementation | `cqs index --force` vs watch requires inter-process file lock | P1 severity, medium implementation |
| PB-NEW-6 fix implementation | Watch + SPLADE full re-encode requires wiring SpladeEncoder through watch context | P1 severity, hard implementation |

The P1-severity hard items (AC-1, DS-W5, PB-NEW-6) stay in P1 for triage-severity accounting but the **work** to fix them spans this cycle and beyond. Track each as an issue with a linked plan doc so the severity doesn't get lost when the implementation slips.

---

## Execution plan

1. **Ship P1 as a single chained PR series.** Group by theme:
   - **PR-A: PR #895 hardening** — CQ-2/3, DS-W1/W3/W4, EH-1/2/3/4, RB-1/2, PF-4, PB-NEW-1/2/3/4/5, RM-3/4, plus the 6 missing SPLADE tests from the adversarial auditor. Meta-fix META-2 (sparse_vectors FK+CASCADE v19 migration) is the architectural backbone.
   - **PR-B: Watch + SPLADE integration** — DS-W2/OB-22/PB-NEW-6 cluster. Short-term: bump generation in watch writes (option B). Long-term: full SPLADE encoder wired into watch (deferred to its own PR).
   - **PR-C: SPLADE hybrid fusion correctness** — AC-1 alone. Touches search/query.rs, risks eval-delta shifts, should ship with re-eval numbers.
   - **PR-D: Documentation that lies** — Doc-1/2/3/11 + the CHANGELOG entry for this whole audit cycle.
   - **PR-E: Flag-declared-but-unread cluster** — CQ-1 + API-1/10/11 + META-1 test.
   - **PR-F: SHL-31/32/33 wiring verification** — promote pre-3.32 SQLite fix to all 15 sites via a shared helper.
   - **PR-G: AC-2/4 router false positives** — word-boundary tokenization in negation and `is_test_chunk`.
   - **PR-H: SEC-NEW-1 reference source containment** — extend SEC-4 validation to cover `source`. Pairs with a SECURITY.md update.
   - **PR-I: DS-W5 + DS-W6** — inter-process lock for `cqs index --force`; double-check under lock for schema migrations.
   - **PR-J: CQ-5 dim-footgun** — `try_load_with_ef(None, Some(store.dim()))` in reference.rs and project.rs.

2. **P2/P3 rolled into release-prep sweep PRs** after P1 ships.

3. **P4 items** get GitHub issues with the relevant audit-findings excerpt pasted in, and links back to this triage doc.

## Next steps

- Generate fix prompts for all P1 items (grouped by PR above). Each prompt: file paths, current code verbatim, replacement code, one-line why.
- Review the fix prompts against source (second-pass agent) to catch drift.
- Execute fixes P1-A through P1-J in dependency order: PR-A (schema+persistence hardening) goes first because META-2 is the foundation the rest assume; PR-B depends on META-2.

---

## Fixed items (session 2026-04-11/12)

| Finding | PR | Status |
|---|---|---|
| AC-2 | #900 | ✅ merged |
| AC-4 | #900 | ✅ merged |
| API-1 | #905 | ✅ merged |
| API-10 | #902 | ✅ merged |
| API-11 | #907 | ✅ merged |
| API-14 | #898 | ✅ merged |
| API-8 | #898 | ✅ merged |
| API-9 | #898 | ✅ merged |
| CQ-1 | #902 | ✅ merged |
| CQ-2 | #898 | ✅ merged |
| CQ-3 | #898 | ✅ merged |
| CQ-5 | #900 | ✅ merged |
| DS-W1 | #898 | ✅ merged |
| DS-W2 | #901 | ✅ merged |
| DS-W3 | #898 | ✅ merged |
| DS-W4 | #898 | ✅ merged |
| DS-W6 | #903 | ✅ merged |
| Doc-1 | #900 | ✅ merged |
| Doc-11 | #900 | ✅ merged |
| Doc-2 | #900 | ✅ merged |
| Doc-3 | #900 | ✅ merged |
| Doc-4 | #900 | ✅ merged |
| Doc-6 | #900 | ✅ merged |
| Doc-7 | #900 | ✅ merged |
| Doc-8 | #900 | ✅ merged |
| Doc-9 | #900 | ✅ merged |
| EH-1 | #898 | ✅ merged |
| EH-2 | #898 | ✅ merged |
| EH-3 | #898 | ✅ merged |
| EH-4 | #898 | ✅ merged |
| EH-5 | #903 | ✅ merged |
| EH-7 | #906 | ✅ merged |
| EH-8 | #907 | ✅ merged |
| EH-9 | #906 | ✅ merged |
| OB-14 | #906 | ✅ merged |
| OB-17 | #898 | ✅ merged |
| OB-19 | #907 | ✅ merged |
| OB-21 | #906 | ✅ merged |
| OB-22 | #901 | ✅ merged |
| PB-NEW-1 | #898 | ✅ merged |
| PB-NEW-2 | #898 | ✅ merged |
| PB-NEW-3 | #898 | ✅ merged |
| PB-NEW-4 | #898 | ✅ merged |
| PB-NEW-5 | #898 | ✅ merged |
| PB-NEW-6 | #901 | ✅ merged |
| PF-4 | #898 | ✅ merged |
| PF-7 | #906 | ✅ merged |
| RB-1 | #898 | ✅ merged |
| RB-2 | #898 | ✅ merged |
| RM-1 | #900 | ✅ merged |
| RM-2 | #903 | ✅ merged |
| RM-3 | #898 | ✅ merged |
| RM-4 | #898 | ✅ merged |
| SEC-NEW-1 | #903 | ✅ merged |
| SEC-NEW-3 | #900 | ✅ merged |
| SHL-31 | #898 | ✅ merged |
| SHL-32 | #904 | ✅ merged |
| SHL-33 | #904 | ✅ merged |

| AC-1 | #910 | ⏳ CI |
| AC-3 | #911 | ⏳ CI |
| API-12 | #911 | ⏳ CI |
| Doc-10 | #911 | ⏳ CI |
| EH-6 | #911 | ⏳ CI |
| OB-13 | #911 | ⏳ CI |
| OB-15 | #911 | ⏳ CI |
| OB-16 | #911 | ⏳ CI |
| OB-18 | #911 | ⏳ CI |
| OB-20 | #908 | ✅ merged |
| PF-6 | #911 | ⏳ CI |
| PF-8 | #911 | ⏳ CI |
| SEC-NEW-2 | #911 | ⏳ CI |
| SHL-34 | #911 | ⏳ CI |
| SHL-35 | #911 | ⏳ CI |
| SHL-36 | #911 | ⏳ CI |
| SHL-37 | #911 | ⏳ CI |
| SHL-38 | #911 | ⏳ CI |
| SHL-39 | #911 | ⏳ CI |
| SHL-40 | #911 | ⏳ CI |

| DS-W5 | #911 | ⏳ CI |

**79 findings fixed out of ~136 triaged.**
All P1 items addressed. AC-1 in PR #910, DS-W5 in PR #911.
