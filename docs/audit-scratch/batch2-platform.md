## Platform Behavior

#### `cqs serve` shutdown_signal handles only Ctrl-C — `systemctl stop` skips graceful drain on Linux
- **Difficulty:** easy
- **Location:** src/serve/mod.rs:253-260
- **Description:** `shutdown_signal()` awaits only `tokio::signal::ctrl_c()`. On Linux when `cqs serve` is run under systemd or any supervisor that issues `SIGTERM` (the default for `systemctl stop`), axum never sees the signal — it keeps serving until systemd escalates to `SIGKILL`. The watch daemon explicitly installs a SIGTERM handler via `libc::signal` (src/cli/watch.rs:132-148), but the serve binary does not. On macOS `launchd` ALSO sends SIGTERM by default. Result: the "press Ctrl-C to stop" banner is the only documented graceful shutdown, and any service-manager wrapper sees forced kills with no graceful_shutdown future polled.
- **Suggested fix:** On `cfg(unix)` race `tokio::signal::ctrl_c()` against `tokio::signal::unix::signal(SignalKind::terminate())` via `tokio::select!`. On Windows also accept `ctrl_break()` and `ctrl_close()`.

#### `EmbeddingCache::default_path` and `QueryCache::default_path` hardcode `~/.cache/cqs/...` on Windows
- **Difficulty:** easy
- **Location:** src/cache.rs:80-84, 1399-1403; src/cli/batch/commands.rs:373-376
- **Description:** Three paths hardcode `dirs::home_dir().join(".cache/cqs/...")`. On Windows this materializes as `C:\Users\X\.cache\cqs\embeddings.db` / `query_cache.db` / `query_log.jsonl`. The native conventions place caches under `%LOCALAPPDATA%\cqs\` (which `dirs::cache_dir()` returns). This is the same defect class as triaged PB-V1.29-8 (HF cache), but PB-V1.29-8 covers HF only; embedding/query caches and the daemon query-log are independent code paths still using the hardcoded layout. Result: Windows users get a hidden `.cache` folder in their home dir that backup tools / antivirus scans don't expect, and dual cqs installs can't share caches with HF tooling that does honor `%LOCALAPPDATA%`.
- **Suggested fix:** Use `dirs::cache_dir().unwrap_or_else(|| dirs::home_dir().join(".cache")).join("cqs")` for all three paths, mirroring `aux_model::hf_cache_dir`'s fallback chain.

#### `dispatch_drift` JSON `file` field is normalized but `dispatch_diff` JSON file fields are not (PB-V1.29-5 partial dupe — additional unfixed sites)
- **Difficulty:** easy
- **Location:** src/suggest.rs:101 (`dead.chunk.file.display().to_string()`), src/store/types.rs:220 (`file_display = file.display().to_string()`)
- **Description:** PB-V1.29-5 covers `dispatch_drift`/`dispatch_diff` in `cli/batch/handlers/misc.rs`. There are at least two more sites that emit Windows backslashes the same way and aren't on the triage list: `suggest::dead_code` returns `Suggestion.file` via `dead.chunk.file.display().to_string()` (rendered into JSON), and `store::types` uses `file_display` for log messages tied to type-edge upserts. Both leak `\` separators into agent-visible output on Windows.
- **Suggested fix:** Replace `.display().to_string()` with `crate::normalize_path(...)` in both sites; add a clippy lint or a doc-tested helper to make the convention discoverable.

#### `serve::open_browser` on Windows passes URL to `explorer.exe` — drops query string / token
- **Difficulty:** medium
- **Location:** src/cli/commands/serve.rs:89-104
- **Description:** `cmd_serve --open` invokes `explorer.exe <url>` on Windows. `explorer.exe` does not interpret a URL argument as a navigation target the way `xdg-open`/`open` do — it tries to open the URL as a path, frequently noops or pops a "Windows can't find" dialog, and on success may strip the `?token=...` query string when handed off to the default browser through DDE. With #1096 auth on by default the token is mandatory; users on Windows lose the one-click experience documented at line 67-82. The other two arms (`xdg-open`, `open`) correctly forward.
- **Suggested fix:** Use `cmd /C start "" "<url>"` on Windows (the empty title is required so `start` parses the URL as the target, not the title). Alternative: use the `opener` crate which already encodes this behavior across platforms.

#### `find_ld_library_dir` splits on `:` — incorrect on Windows / wrong env var name
- **Difficulty:** easy
- **Location:** src/embedder/provider.rs:115-123
- **Description:** `find_ld_library_dir` is `cfg(target_os = "linux")`-gated, so this is currently dormant. But the `ensure_ort_provider_libs` helper has only a Linux arm — there is no equivalent for Windows or macOS. Consequence: when ORT ships on Windows targets the only fallback for finding provider DLLs is the system loader's PATH search, with no logging of where it actually looked. Documenting the gap in the function header and adding a Windows arm that walks `PATH` (split on `;`, looking for `onnxruntime_providers_*.dll`) makes the cross-platform CUDA story explicit instead of "Linux works, others get whatever ORT happens to do."
- **Suggested fix:** Either add `#[cfg(target_os = "windows")]` arms to `ensure_ort_provider_libs` / `find_ort_provider_dir` that walk `PATH` with `;` separator and look for `.dll`, or add a top-level doc comment stating the Windows path resolution is delegated entirely to ORT and confirming the release CI tests catch the failure mode.

#### `ProjectRegistry` doc claims `~/.config/cqs/projects.toml` but `dirs::config_dir()` returns macOS-specific path
- **Difficulty:** easy
- **Location:** src/project.rs:1-3, 176-179
- **Description:** Module-level doc says "Maintains a registry of indexed projects at `~/.config/cqs/projects.toml`". On macOS `dirs::config_dir()` returns `~/Library/Application Support/`, so the actual file lives at `~/Library/Application Support/cqs/projects.toml`. On Windows it lives at `%APPDATA%\cqs\projects.toml`. macOS and Windows users following the doc to find / edit the registry will look in the wrong place. The path is constructed correctly via `dirs::config_dir()` — only the doc is lying. (Per memory note `feedback_docs_lying_is_p1.md`: docs lying about a path users will run `ls`/`open` on is a P1 correctness bug, not "just docs".)
- **Suggested fix:** Update both the module doc and the `load`/`save` doc comments to enumerate the three platform paths, e.g. "Linux: `~/.config/cqs/`, macOS: `~/Library/Application Support/cqs/`, Windows: `%APPDATA%\cqs\`". Mention `dirs::config_dir()` as the source of truth.

#### `index.lock` `flock` is advisory on Linux but mandatory on Windows — different failure modes for cross-tooling
- **Difficulty:** medium
- **Location:** src/cli/files.rs:120-213
- **Description:** `acquire_index_lock` uses `std::fs::File::try_lock` (introduced in Rust 1.89, MSRV 1.93+). On Linux this maps to `flock(LOCK_EX|LOCK_NB)` — purely advisory; non-cqs writers (e.g. an editor saving the DB after a crash, an external SQLite tool) will silently corrupt the index. On Windows it maps to `LockFileEx` which is mandatory and prevents *any* other process from opening the file with a conflicting share mode — including a benign `sqlite3.exe` or backup tool that opens with `FILE_SHARE_READ` but no `FILE_SHARE_WRITE`. The function-level doc covers the WSL `/mnt/c` case but does not document the Linux-vs-Windows mandatory-vs-advisory difference, and `is_wsl_drvfs_path` is not consulted before deciding to trust the lock. Result: same code, two very different concurrency contracts that callers cannot distinguish at runtime.
- **Suggested fix:** Add a `tracing::warn!` once at startup on Windows noting that the lock is mandatory and that opening `index.db` from another process while the lock is held will fail with sharing violation. Document the Linux/Windows split in the `acquire_index_lock` doc-comment alongside the existing WSL paragraph.

#### `is_wsl_drvfs_path` only matches single-letter drive mounts — misses `wsl.localhost` and explicit-uppercase mounts
- **Difficulty:** easy
- **Location:** src/config.rs:92-101
- **Description:** The pattern requires exactly `/mnt/<lowercase letter>/`. WSL2 also exposes Windows drives under `//wsl.localhost/<distro>/mnt/c/...` and (when accessed from the Windows side) `\\wsl$\<distro>\mnt\c\...`. Additionally, `wsl.conf` `automount.options=case=force` allows uppercase drive letters. The `cli/watch.rs::create_watcher` code at line 1483-1489 already explicitly checks for `//wsl` and `is_under_wsl_automount`, but the *shared* helper used by config / project / hnsw doesn't, so those three sites still treat WSL DrvFS paths reached via UNC as native Linux. They'll then warn about world-readable perms (line 497-503) on a path where Linux-side perms are meaningless.
- **Suggested fix:** Extend `is_wsl_drvfs_path` to also match `//wsl.localhost/`, `//wsl$/`, and uppercase drive letters. Test via `daemon_translate` style tests that fix `WSL_DISTRO_NAME`.

#### `git_file = rel_file.replace('\\', "/")` only normalizes one direction — Windows-origin chunk IDs slip through
- **Difficulty:** easy
- **Location:** src/cli/commands/io/blame.rs:113-115
- **Description:** Comment says "PB-3: Windows compat" and the `replace('\\', "/")` covers the common case where chunk.file came from `cqs::normalize_path` (forward-slash). But `chunk.file` can also be a `PathBuf` whose components include the verbatim `\\?\` prefix when the chunk was inserted from a path that bypassed `normalize_path` (e.g. a partial / pre-DS2-1 fix path). The replace would emit `//?/C:/Projects/...` to git, which git rejects with "ambiguous argument". A symmetric strip via `crate::normalize_slashes(&rel_file)` (which calls `strip_windows_verbatim_prefix` first) is what the rest of the codebase uses.
- **Suggested fix:** `let git_file = crate::normalize_slashes(&rel_file);` — covers both backslash conversion and `\\?\` strip in one call, matching the convention established in src/lib.rs:420.

#### `daemon_socket_path` falls back to `std::env::temp_dir()` on `XDG_RUNTIME_DIR` unset — different parent-dir trust on macOS
- **Difficulty:** medium
- **Location:** src/daemon_translate.rs:179-188
- **Description:** Linux desktops set `XDG_RUNTIME_DIR=/run/user/<uid>` (mode 0700, owned by the user). When unset (headless servers, container minimal images, macOS where `XDG_RUNTIME_DIR` is not standard), the code falls back to `std::env::temp_dir()` — `/tmp` on Linux, `/var/folders/.../T` on macOS. macOS's `/var/folders/...` is per-user-and-bootstrap and reasonably private (mode 0700), but Linux `/tmp` is mode 1777. The umask wrap at watch.rs:1626 narrows the bind window, and the explicit `chmod 0o600` at watch.rs:1637 is the actual access gate. Still, the silent fallback hides a meaningful trust boundary: on a Linux multi-user system without `XDG_RUNTIME_DIR`, the socket lives in a directory where another local user can `unlink` it (or `mkfifo` over its name during the bind race). The doc comment notes the issue (line 1615-1622 SEC-D.6) but `daemon_socket_path` itself doesn't log when the fallback fires.
- **Suggested fix:** When `XDG_RUNTIME_DIR` is unset on Linux, log `tracing::info!("XDG_RUNTIME_DIR unset — daemon socket falls back to temp_dir; consider setting XDG_RUNTIME_DIR=/run/user/$(id -u)")` once per process. On macOS the fallback is fine — gate the warning on `cfg(target_os = "linux")` so it's only emitted where `/tmp` is actually shared.

#### NTFS mtime resolution is 100ns but Windows-side editors update mtime at 2s granularity in some configurations — `prune_last_indexed_mtime` watermark too tight
- **Difficulty:** medium
- **Location:** src/cli/watch.rs:551-560 (and wider mtime-keyed change-detection in `should_reindex`)
- **Description:** `last_indexed_mtime` is a `HashMap<PathBuf, SystemTime>` and the watcher decides "skip unchanged mtime" via exact `SystemTime` equality. NTFS file timestamp resolution is documented as 100ns, but FAT32 (still mounted on USB sticks, recovery partitions, and some `/mnt/<letter>/` paths) has 2-second resolution on writes. WSL DrvFS exposes the underlying NTFS mtime, but Windows-side `notepad.exe` saves can lose sub-second precision when the underlying filesystem is FAT32. Two saves within 2s on a FAT32 mount will therefore collide on the same mtime and the watch loop will skip the second — a real correctness gap on `/mnt/d` if D: is a USB stick. There's a 1s debounce auto-bump on WSL DrvFS (line 1495-1500) that masks most of this, but the equality check against the cached mtime doesn't.
- **Suggested fix:** When `is_wsl_drvfs_path(file)` is true, treat mtime equality with `<` instead of `==` over a 2-second buckets, OR fall back to content-hash comparison (already computed for parser ingest) when mtime equality is suspicious. Document the FAT32 caveat in the function header.

#### `serve::enforce_host_allowlist` accepts missing Host header — dev-only ergonomic leaks into production
- **Difficulty:** easy
- **Location:** src/serve/mod.rs:230-251
- **Description:** Comment at lines 230-233 explains the bypass: "A missing `Host:` header passes through — HTTP/1.1 requires one and hyper always provides one on real traffic, but unit tests built via `Request::builder()` without a `.uri()` that includes a host don't get one synthesized, and we'd rather not break that ergonomic." That's a unit-test ergonomic baked into the production middleware. A non-browser HTTP/1.0 client (or HTTP/2 client that uses `:authority` but routes through a proxy that strips it) reaches the handler with no Host header, bypassing the DNS-rebinding allowlist that SEC-1 closes for browser traffic. The auth token (#1096) covers this in default config, but `--no-auth` exposes it.
- **Suggested fix:** In production code reject missing Host with 400, and in `tests.rs` add `Host: localhost` to the `Request::builder()` fixtures (cheap one-liner). Or gate the bypass on `cfg(test)`.
