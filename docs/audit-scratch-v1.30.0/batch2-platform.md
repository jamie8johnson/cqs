## Platform Behavior

#### PB-V1.29-1: `cqs context`/`cqs brief` fail on Windows when user types path with backslashes
- **Difficulty:** easy
- **Location:** `src/cli/commands/io/context.rs:28,115` + `src/cli/commands/io/brief.rs:40-42`
- **Description:** `cmd_context` and `cmd_brief` pass raw CLI `path: &str` through to `store.get_chunks_by_origin(path)`, which binds `WHERE origin = ?1`. The DB only ever stores forward-slash origins (enforced by `normalize_path` + the `debug_assert!(!origin.contains('\\'))` at `staleness.rs:589-592`). On Windows, a user / agent running `cqs context src\foo.rs` will get `"No indexed chunks found"` even when the file is indexed. `cmd_reconstruct` at `reconstruct.rs:32-39` proves the correct pattern (it calls `cqs::normalize_path` first); `cmd_context` and `cmd_brief` were missed.
- **Suggested fix:** Normalize the user-supplied path before lookup:
  ```rust
  let path_norm = cqs::normalize_slashes(path);
  let chunks = store.get_chunks_by_origin(&path_norm)?;
  ```
  Apply the same in `cmd_brief`, `dispatch_context`, and anywhere else that forwards CLI path to `get_chunks_by_origin[s_batch]`.

#### PB-V1.29-2: Watch SPLADE encoder passes Windows `file.display()` to `get_chunks_by_origin` — silent no-op
- **Difficulty:** easy
- **Location:** `src/cli/watch.rs:1083-1085`
- **Description:**
  ```rust
  for file in changed_files {
      let origin = file.display().to_string();
      let chunks = match store.get_chunks_by_origin(&origin) { ... };
  ```
  On Windows `PathBuf::display()` emits the verbatim `\\?\C:\...` prefix for canonicalized paths AND backslash separators. DB origins are stored as relative + forward-slash via `normalize_path`. So `get_chunks_by_origin(&origin)` returns `Ok(vec![])` on Windows, `encode_splade_for_changed_files` silently produces an empty batch, and `cqs watch --serve` never updates the SPLADE index for any modified file on Windows. Silent correctness failure.
- **Suggested fix:** Use the project-relative, forward-slash form consistently. Use `cqs::normalize_path(file)` (or the already-computed `rel_path` from the caller's loop) rather than `file.display().to_string()`. Re-verify with `RUST_LOG=debug` after fix on a Windows/WSL+DrvFS test path.

#### PB-V1.29-3: `chunk.id` prefix-strip uses `abs_path.display()` — breaks on Windows verbatim + backslash paths
- **Difficulty:** medium
- **Location:** `src/cli/watch.rs:2432-2434`
- **Description:**
  ```rust
  if let Some(rest) = chunk.id.strip_prefix(&abs_path.display().to_string()) {
      chunk.id = format!("{}{}", rel_path.display(), rest);
  }
  ```
  `chunk.id` format is `{path}:{line_start}:{content_hash}` where `{path}` is assigned during parsing. If the parser produced a forward-slash id (matching the rest of the codebase's normalization), `strip_prefix(abs_path.display())` fails on Windows (where display emits backslashes / `\\?\` verbatim) and the id keeps the absolute path. Chunks then end up with ids that don't match the relative-path ids seen everywhere else in the index, breaking `cqs read`, `cqs context`, `cqs callers` joins — silent data integrity drift on incremental indexing. The same file full-re-indexed produces correctly-prefixed ids, so stale/fresh chunks mix.
- **Suggested fix:** Normalize both sides through `cqs::normalize_path`:
  ```rust
  let abs_str = cqs::normalize_path(&abs_path);
  if let Some(rest) = chunk.id.strip_prefix(&abs_str) {
      chunk.id = format!("{}{}", cqs::normalize_path(&rel_path), rest);
  }
  ```
  Add a regression test that covers `chunk.id` containing backslashes + `\\?\` prefix.

#### PB-V1.29-4: `init` writes `.gitignore` with LF-only, breaks `git status` on Windows `core.autocrlf=true`
- **Difficulty:** easy
- **Location:** `src/cli/commands/infra/init.rs:36-40`
- **Description:** Finding PB-V1.25-8 from the v1.25.0 triage is still pending:
  ```rust
  std::fs::write(
      &gitignore,
      "index.db\nindex.db-wal\n...\n",
  )
  ```
  On a Windows git checkout with `core.autocrlf=true` (default on Git-for-Windows), `git status` immediately shows `.cqs/.gitignore` as modified because Git re-writes it with CRLF endings. The file is not even under source control (lives in `.cqs/`) but agents running on Windows get noise in `cqs blame` / `cqs diff` on any working-tree inspection.
- **Suggested fix:** Either (a) write platform-native line endings via `#[cfg(windows)]` replacing `"\n"` with `"\r\n"`, or (b) avoid autocrlf detection via a `.gitattributes` sibling in `.cqs/` marking `* -text`. Option (a) is the least-surprising fix.

#### PB-V1.29-5: JSON path fields emit Windows backslashes — breaks cross-platform agent consumers
- **Difficulty:** easy
- **Location:** `src/cli/batch/handlers/misc.rs:277,353,365,377`
- **Description:** `dispatch_drift` + `dispatch_diff` serialize result file paths as `e.file.display().to_string()`:
  ```rust
  "file": e.file.display().to_string(),
  ```
  On Windows, this emits `src\foo.rs` in the JSON envelope. The rest of the codebase normalizes to forward slashes via `cqs::normalize_path` / `serialize_path_normalized`. An agent that reads `cqs drift --json` then uses the `file` field for a follow-up `cqs impact` / `cqs read` call will feed a backslash path into `get_chunks_by_origin` — which (per PB-V1.29-1) returns nothing.
- **Suggested fix:** Use the existing helper. Either inline `cqs::normalize_path(&e.file)` at each site, or have the structs use `#[serde(serialize_with = "cqs::serialize_path_normalized")]` on their `PathBuf` fields.

#### PB-V1.29-6: Hardcoded `/mnt/` WSL check in `hnsw/persist.rs` + `project.rs` — ignores custom `wsl.conf automount.root`
- **Difficulty:** easy
- **Location:** `src/hnsw/persist.rs:86-87` + `src/project.rs:85-86` + `src/config.rs:445-451`
- **Description:** Three independent WSL-mount checks spot-probe the default `/mnt/` prefix:
  ```rust
  if crate::config::is_wsl()
      && dir.to_str().is_some_and(|p| p.starts_with("/mnt/"))
  ```
  WSL allows users to customize the automount root in `/etc/wsl.conf` (e.g. `root = /windows/`). Only `cli/watch.rs::is_under_wsl_automount` parses `wsl.conf` to handle this (PB-3 fix). The HNSW advisory-locking warning, project-registry locking warning, and config-permission-skip branch all silently miss non-default automount roots. On such a system the WSL file-locking advisory warning never fires and the permission-check spams warnings at users whose NTFS mount lives at `/windows/c/...`.
- **Suggested fix:** Lift `is_under_wsl_automount` out of `cli/watch.rs` into `cqs::config` and use it in all four sites (including the existing wsl.conf parser). Alternatively, detect DrvFS specifically via `statfs` magic number (9P=0x01021997 / DrvFS has its own signature).

#### PB-V1.29-7: `EmbeddingCache::open` / `QueryCache::open` propagate `set_permissions(0o700)` on WSL `/mnt/c/` — cache open can fail spuriously
- **Difficulty:** easy
- **Location:** `src/cache.rs:73-80, 1002-1009`
- **Description:**
  ```rust
  if let Some(parent) = path.parent() {
      std::fs::create_dir_all(parent)?;
      #[cfg(unix)]
      {
          use std::os::unix::fs::PermissionsExt;
          std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
      }
  }
  ```
  The `?` propagates the permissions-set failure. On WSL DrvFS (`/mnt/c/`), NTFS doesn't honor POSIX mode bits — `set_permissions` returns `EINVAL` or succeeds as no-op depending on kernel/DrvFs version. If the cache file lives on `/mnt/c/` (because `dirs::home_dir()` points at a Windows HOME under WSL) the `?` kills the entire cache-open with a "permission denied" error and the binary falls back to un-cached queries (or crashes an indexing pipeline). Same pattern was already fixed in PB-V1.25-15 for the daemon socket (`.ok()` → explicit warn). Asymmetric.
- **Suggested fix:** Downgrade to best-effort with a warn-on-failure. Mirror the pattern at `cache.rs:145-150`:
  ```rust
  if let Err(e) = std::fs::set_permissions(parent, ...) {
      tracing::warn!(path = %parent.display(), error = %e,
          "Failed to tighten cache parent dir permissions (WSL DrvFs / NTFS?); continuing");
  }
  ```

#### PB-V1.29-8: `HF_HOME` / `HUGGINGFACE_HUB_CACHE` env lookup doesn't honor Windows `%LOCALAPPDATA%` default
- **Difficulty:** medium
- **Location:** `src/cli/commands/infra/doctor.rs:858-868` + `src/splade/mod.rs:957` + `src/aux_model.rs:181,188,446,528`
- **Description:** All of these hardcode `~/.cache/huggingface/...`:
  ```rust
  dirs::home_dir().map(|h| h.join(".cache/huggingface/hub"))
  ```
  The HuggingFace SDK docs state the Windows default is `%USERPROFILE%\.cache\huggingface\hub`. This is mostly right — on Windows `dirs::home_dir()` → `%USERPROFILE%` so the joined path works *if* Windows users keep the HF defaults. However: Windows users who installed Python+transformers got `%LOCALAPPDATA%\huggingface\hub` from older `huggingface_hub` versions, and the conventional Windows cache root has always been `%LOCALAPPDATA%`. `cqs doctor --json` will display a non-existent path on Windows as the "expected HF cache", making the "Model not downloaded" diagnostic misleading.
- **Suggested fix:** Use `dirs::cache_dir()` (which resolves correctly per-OS) joined with `huggingface/hub` as the fallback:
  ```rust
  if let Ok(p) = std::env::var("HF_HOME") { return PathBuf::from(p).join("hub"); }
  if let Ok(p) = std::env::var("HUGGINGFACE_HUB_CACHE") { return PathBuf::from(p); }
  dirs::cache_dir().map(|c| c.join("huggingface/hub"))
      .or_else(|| dirs::home_dir().map(|h| h.join(".cache/huggingface/hub")))
      .unwrap_or_else(|| PathBuf::from(".cache/huggingface/hub"))
  ```
  (still falls through to `~/.cache/huggingface/hub` for Linux/macOS/WSL where that is the documented default.)

#### PB-V1.29-9: `aux_model::expand_tilde` only handles `~/` prefix — misses `~` alone and native Windows `%USERPROFILE%`
- **Difficulty:** easy
- **Location:** `src/aux_model.rs:101-108`
- **Description:**
  ```rust
  fn expand_tilde(raw: &str) -> PathBuf {
      if let Some(stripped) = raw.strip_prefix("~/") {
          if let Some(home) = dirs::home_dir() { return home.join(stripped); }
      }
      PathBuf::from(raw)
  }
  ```
  A user configuring `splade.model_path = "~"` (bare tilde, pointing at home) fails expansion. More importantly, Windows users using `~\Models\splade` (backslash separator) are not expanded, and `cqs-<version>/.cqs.toml` is silently treated as a literal path starting with `~\`. On the `is_path_like` check at line 124, `raw.starts_with("~/")` is also Windows-blind.
- **Suggested fix:** Extend the check to handle `~` alone, `~/`, and `~\` (plus `$HOME` / `%USERPROFILE%` if symmetry is desired):
  ```rust
  fn expand_tilde(raw: &str) -> PathBuf {
      if raw == "~" { return dirs::home_dir().unwrap_or_else(|| PathBuf::from(raw)); }
      if let Some(rest) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix(r"~\")) {
          if let Some(home) = dirs::home_dir() { return home.join(rest); }
      }
      PathBuf::from(raw)
  }
  ```
  Apply the same extension to `is_path_like`.

#### PB-V1.29-10: WSL detection via `/proc/version` — misses native Linux containers with "microsoft"/"wsl" in kernel string
- **Difficulty:** medium
- **Location:** `src/config.rs:31-47`
- **Description:**
  ```rust
  pub fn is_wsl() -> bool {
      static IS_WSL: OnceLock<bool> = OnceLock::new();
      *IS_WSL.get_or_init(|| {
          if std::env::var_os("WSL_DISTRO_NAME").is_some() { return true; }
          std::fs::read_to_string("/proc/version")
              .map(|v| v.to_lowercase().contains("microsoft") || v.contains("wsl"))
              .unwrap_or(false)
      })
  }
  ```
  Two issues:
  1. `v.contains("wsl")` is case-sensitive on the second predicate but `.to_lowercase()` was applied only to the first; line 42 stores `.to_lowercase()` in `lower` and checks both, so this particular bug is not active — but the test comparing raw `v` (if refactored) would silently regress.
  2. The detection can false-positive on Linux hosts where `/proc/version` happens to mention Microsoft (e.g. Mariner Linux, some Azure images, or a custom kernel with a "Microsoft" contributor in `CONFIG_CC_VERSION_TEXT`). On those hosts `cqs` then switches to `--poll` mode and bumps debounce to 1500ms for no reason, slowing watch cycles ~3×.
- **Suggested fix:** Also require `WSL_INTEROP` or `/run/WSL` / `/proc/sys/fs/binfmt_misc/WSLInterop` to be present. Those are set exclusively by the WSL init process. Falling back to `/proc/version` alone is the cheapest signal but should not be the only one.

---

## Summary
10 findings: 8 easy + 2 medium. Most are path-normalization gaps where forward-slash DB origins meet backslash or verbatim-prefix user/system input on Windows or WSL. PB-V1.29-2 (silent SPLADE no-op on Windows watch) and PB-V1.29-3 (chunk.id drift on incremental re-index) are the highest-impact correctness issues.
