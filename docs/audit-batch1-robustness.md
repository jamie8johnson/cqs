## Robustness

Audit pass against the post-v1.38.0 main branch (4a31285e). RB-V1.36-*
items from prior triage are excluded. Production unwrap surface is small
(~25 sites), but a handful of latent panics live in user/external-input
paths (worktree, daemon reindex, SPLADE on-disk index, malicious config
files) and a few `unwrap_or` patterns are sibling-level misses of
already-fixed bounds.

#### RB-V1.38-1: `worktree::resolve_main_project_dir` reads `commondir` unbounded — sibling of `RB-V1.33-2`
- **Difficulty:** easy
- **Location:** `src/worktree.rs:91`
- **Description:** `MAX_GIT_FILE_BYTES = 4 KiB` is correctly applied to the
  `.git` link file at line 67–71 (`File::open ... .take(MAX_GIT_FILE_BYTES)`),
  but the very next read — `<gitdir>/commondir` at line 91 — uses
  `std::fs::read_to_string(&commondir_file)` with no cap. `commondir` is a
  git-internal file that normally contains `../..` (~6 bytes), but the path
  is computed from the worktree's untrusted `.git` file (`gitdir:` line) and
  fed to `read_to_string`. A worktree pointing at a hostile gitdir whose
  `commondir` is a multi-GB file (or a FIFO) OOMs / hangs every cqs command
  invoked from inside the worktree. Resolves on every CLI call that hits
  `resolve_index_dir`, so this fires on cold-paths the daemon can't shield.
- **Suggested fix:** Same shape as the `.git` reader above —
  `File::open(&commondir_file).ok()?.take(MAX_GIT_FILE_BYTES).read_to_string(&mut buf).ok()?`.
  Real `commondir` content is < 100 bytes; 4 KiB cap is ample.

#### RB-V1.38-2: `cli/watch/reindex.rs:626` panics the daemon on chunk-index mismatch
- **Difficulty:** medium
- **Location:** `src/cli/watch/reindex.rs:617-627`
- **Description:** Watch-mode reindex merges `cached` and `to_embed` into a
  `HashMap<usize, Embedding>` keyed by chunk index, then rebuilds the
  per-chunk vector with `(0..chunk_count).map(|i| by_index.remove(&i).unwrap_or_else(|| panic!(...)))`.
  The comment claims it's unreachable, but a partial embedder failure where
  `new_embeddings.len() != to_embed.len()` (e.g. ORT session error mid-batch,
  or any future code path that returns a short Vec) lands directly in the
  panic arm and kills the daemon. The watch loop is the daemon's hot path —
  a single bad embedder run takes down `cqs-watch` until systemd restarts.
- **Suggested fix:** Return `Err(WatchError::ReindexInvariant { chunk_index, chunk_count })`
  via `?` so the watch loop logs the violation and skips this file rather
  than crashing. The caller already handles per-file errors; the panic is
  needlessly fatal for a recoverable invariant break.

#### RB-V1.38-3: `splade/index.rs` load-path uses `.try_into().unwrap()` 9× on body slices
- **Difficulty:** easy
- **Location:** `src/splade/index.rs:703,710,717,718,719,781,819,822,828,830`
- **Description:** Each of these is `u32::from_le_bytes(slice[a..b].try_into().unwrap())`
  (or `u64`/`f32`/`[u8; 32]` variants) on slices read from a SPLADE on-disk
  index. By construction every site is preceded either by the
  fixed-`SPLADE_INDEX_HEADER_LEN` header read or by a `need(&body, cursor, n)?`
  bound check, so the unwraps are provably safe today. They violate the
  project's "no `unwrap()` outside tests" rule and silently lose the audit
  trail — a future refactor that drops a `need()` call leaves no compile-time
  signal that the corresponding `try_into().unwrap()` becomes a panic on a
  malformed index. SPLADE indexes are loaded from disk (`.cqs/splade.idx`),
  so the input is reachable by anyone who can write to `.cqs/`.
- **Suggested fix:** Replace each with
  `u32::from_le_bytes(slice[a..b].try_into().expect("invariant: header[4..8] is exactly 4 bytes"))`
  to surface the invariant in the binary's panic messages, OR thread the
  `try_into()` result into the existing `SpladeIndexPersistError::CorruptData`
  return path with a single helper (`fn read_le_u32(body: &[u8], cursor: usize) -> Result<u32, ...>`).
  The helper version costs one closure call per field and removes the panic
  surface entirely.

#### RB-V1.38-4: `cli/commands/infra/doctor.rs:582` `api_base.unwrap()` is brittle by-flow-only
- **Difficulty:** easy
- **Location:** `src/cli/commands/infra/doctor.rs:582`
- **Description:** `let base = api_base.unwrap();` is reachable today only
  because the `match api_base.as_deref() { _ => return; ... }` block at
  lines 547-563 returns on `None`. The unwrap holds because of an earlier
  early-return, not because of a type-level guarantee — a future refactor
  that turns the early return into a `*any_failed = true; ` (without
  `return`) silently converts this into a panic on `cqs doctor` (a user-facing
  diagnostic command — it must not crash). This is exactly the pattern
  CLAUDE.md flags as an anti-pattern.
- **Suggested fix:** Hoist the value out of the first match instead of
  re-reading: `let base = match api_base.as_deref() { Some(s) if !s.is_empty() => s.to_string(), _ => { /* current err branch */ return; } };`.
  This makes the value's existence a type-level fact at line 582.

#### RB-V1.38-5: `cli/commands/index/umap.rs:122` capacity computation overflows on pathological input
- **Difficulty:** medium
- **Location:** `src/cli/commands/index/umap.rs:122`
- **Description:** `Vec::with_capacity(12 + n_rows * (2 + id_max_len + dim * 4))`.
  The `ensure!` block above only validates each operand fits in `u32` (≤
  4.3e9). On a 64-bit host the product can reach ~9.2e19, well past
  `usize::MAX` (1.8e19). In release builds usize multiplication wraps
  silently and `Vec::with_capacity` allocates a misleadingly small buffer
  before `extend_from_slice` panics on out-of-bounds memory. In debug builds
  the multiplication panics directly. The current corpus is far from this
  bound, but the validation is in u32-space rather than product-space, so a
  malicious / huge index can drive the multiplication into UB territory.
- **Suggested fix:** Replace with checked arithmetic and `anyhow::ensure!`
  before `with_capacity`:
  `let cap = 12usize.checked_add(n_rows.checked_mul(2usize.checked_add(id_max_len)?.checked_add(dim.checked_mul(4)?)?)?).ok_or_else(|| anyhow::anyhow!("UMAP payload size overflows usize"))?;`
  Or simply pre-compute on `u128` and reject if `cap > 1 GiB` (the actual
  IPC budget).

#### RB-V1.38-6: `parser/l5x.rs` regex-capture `.unwrap()` cluster (6 sites)
- **Difficulty:** easy
- **Location:** `src/parser/l5x.rs:263,264,366,367,368,387`
- **Description:** Six `.get(N).unwrap()` calls on `regex::Captures`:
  ```rust
  let full = st_match.get(0).unwrap();
  let inner = st_match.get(1).unwrap();
  ...
  let routine_name = block.get(1).unwrap().as_str().to_string();
  let block_content = block.get(2).unwrap().as_str();
  let block_start = block.get(0).unwrap().start();
  ...
  let inner = st_block.get(1).unwrap().as_str();
  ```
  Every regex involved (`L5X_ST_CONTENT_RE`, `L5K_ROUTINE_BLOCK_RE`,
  `L5K_ST_CONTENT_BLOCK_RE`) defines unconditional capture groups, so groups
  1 and 2 are always present when the parent match exists — the unwraps are
  semantically safe. They still violate the project rule and would not
  survive a regex tweak that adds an alternation or makes a group optional.
  L5X parsing is invoked on every Rockwell `.l5x` / `.l5k` source the user
  hands cqs (via `cqs index`); a hostile/corrupt file with a regex-quirk
  edge case (zero-width match, anchor weirdness) could expose an
  inconsistency.
- **Suggested fix:** Replace with `match block.get(1) { Some(m) => m.as_str().to_string(), None => continue, }` (skip the malformed routine, parser already accepts partial input). For `block.get(0)` use `let block_start = st_match.get(0).map(|m| m.start()).unwrap_or(0);`.

#### RB-V1.38-7: `train_data/query.rs:14` static regex `.unwrap()` inconsistent with siblings
- **Difficulty:** easy
- **Location:** `src/train_data/query.rs:14`
- **Description:** Three `LazyLock<Regex>` siblings in the same file
  (`conventional_prefix_re`, `trailing_noise_re`, lines 7 and 21) use
  `.expect("valid regex")`; the leading-verb regex at line 14 uses bare
  `.unwrap()`. The regex is a 90+ alternation built by hand — if a typo
  ever lands (an unbalanced `(`, a malformed character class), the panic
  message gives no breadcrumb. Trivial inconsistency, easy to fix, matches
  existing style.
- **Suggested fix:** `Regex::new(...).expect("valid leading-verb regex")`.

#### RB-V1.38-8: `embedder/models.rs:684,691,741` `.expect("guarded by has_X")` on `Option`
- **Difficulty:** easy
- **Location:** `src/embedder/models.rs:684,691,741`
- **Description:** Three sites: `embedding_cfg.dim.expect("guarded by has_dim")`,
  `embedding_cfg.repo.as_ref().expect("guarded by has_repo")`,
  `embedding_cfg.repo.clone().expect("guarded by has_repo")`. The `has_*`
  flags are local `bool` variables computed at lines 681-682 with `.is_some()`
  and gated by `if has_repo && has_dim`, so the expect calls are safe today.
  But the guard and the unwrap are in different scopes and there's no
  type-level invariant — a future contributor refactoring the validation
  block (e.g., adding a dim==0 check that mutates `dim` to None on the way
  through) could silently turn the expect into a panic on user-supplied
  `cqs.toml`. Custom-model TOML config is **user input**, so this is a
  practical robustness concern.
- **Suggested fix:** Bind the unwrapped values once at the top of the
  validation block: `let (Some(repo), Some(dim)) = (&embedding_cfg.repo, embedding_cfg.dim) else { return Self::default_model(); };` (or equivalent if-let-chain), then use `repo` and `dim` directly throughout. Removes the guard/unwrap split.

#### RB-V1.38-9: `nl/fields.rs:122` `unreachable!()` reachable on field-style additions
- **Difficulty:** easy
- **Location:** `src/nl/fields.rs:122`
- **Description:** `FieldStyle::None => unreachable!()` is genuinely
  unreachable today — line 84 returns early on `FieldStyle::None`. But the
  match at line 95 is non-exhaustive over future variants: if a fourth
  field style is added to `FieldStyle` without a return-early shortcut at
  line 84, the match falls through to `unreachable!()` and panics on every
  source file in that language. `FieldStyle` is a project-internal enum
  that new languages routinely touch; the early-return discipline is
  enforced at runtime, not by the type system.
- **Suggested fix:** Either swap to `_ => return Vec::new()` (matches the
  line-84 fall-through behavior), or move the `FieldStyle::None` arm into
  the match itself (`FieldStyle::None => return None` inside the match). The
  latter restores exhaustiveness so a new variant is a compile error, not
  a runtime panic.

#### RB-V1.38-10: `cli/watch/mod.rs:1654,1695` `handle_opt.take().unwrap()` brittle to refactor
- **Difficulty:** easy
- **Location:** `src/cli/watch/mod.rs:1654,1695`
- **Description:** Both call sites are inside a
  `match handle_opt.as_ref() { Some(h) if h.is_finished() => { handle_opt.take().unwrap().join() ... } }`
  pattern. The unwrap holds because the `Some(h)` arm guarantees the
  Option is `Some` at the point `take()` is called — but only because
  Rust treats `.as_ref()` views and `.take()` operations on the same
  binding as referring to the same value. A refactor that turns
  `handle_opt` into a re-bound value or that adds an early intervening
  `take()` (e.g. for a deadline-cancellation check) silently turns this
  into a daemon-shutdown panic. The daemon is mid-shutdown when this
  fires, so a panic during shutdown can leave socket files / lock files
  orphaned (see existing `daemon_socket` cleanup ordering). The watch
  module is the project's most operationally sensitive component.
- **Suggested fix:** Use `if let Some(handle) = handle_opt.take()` directly
  instead of the match-arm-then-take-unwrap dance:
  ```rust
  if let Some(handle) = handle_opt.take() {
      if handle.is_finished() {
          if let Err(e) = handle.join() { ... }
          break;
      } else {
          handle_opt = Some(handle);  // put it back
      }
  }
  ```
  Or split into a `try_join_with_deadline` helper that owns the join
  semantics and never exposes the unwrap to the call site.

