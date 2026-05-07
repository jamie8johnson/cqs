## Platform Behavior — post-v1.36.2 (v1.38.0 + post-v1.38 work)

Scope: NEW sites added since the v1.36.2 audit. Re-confirmed `audit-findings.md` lines 673-733
already cover: SEC-1 audit-mode/project.rs Windows ACL gap (P4-19), `db_file_identity` Windows
mtime fallback (P4-17), `tasklist` UTF-16 BOM (P4-10 — fixed in #1490), daemon `cfg(unix)` fence
(P4-9 / #1512), `lookup_main_cqs_dir` non-canonicalization, `strip_prefix` leakage. Skipped.

---

#### PL-V1.38-1: CAGRA `.cagra` blob is born world-readable on Unix — SEC-1 contract gap vs HNSW
- **Difficulty:** easy
- **Location:** `src/cagra.rs:1341` (`gpu.index.serialize(...)` → tmp file written by cuVS), promoted via `crate::fs::atomic_replace` at line 1385 with no chmod
- **Description:** `save_blob_atomic_with_rollback` lets cuVS create the `.cagra.tmp` file via FFI; cuVS uses default umask (typically 0o644). Unlike `hnsw/persist.rs:458-470` which explicitly `set_permissions(0o600)` on `.hnsw.graph` / `.hnsw.data` AFTER cuVS-equivalent (here `hnsw_rs`) writes them and BEFORE rename, CAGRA's save path skips the chmod entirely. The promoted `index.cagra` ends up world-readable on multi-user Linux; the `.bak` rollback file (created by `rename` at line 1357) inherits the same loose mode. Anyone in the `cqs` group can read another user's vector index — which contains the embedded chunk content effectively (graph topology + dim + len). Same SEC-1 promise SECURITY.md / cache.rs / store.rs / hnsw/persist.rs all enforce; CAGRA is the lone exception. Not flagged in v1.36.2 because the `.bak` rollback work (#1492) was post-audit.
- **Suggested fix:** After line 1414 (successful `atomic_replace`) and again after `bak_path` rename at 1357, `#[cfg(unix)] std::fs::set_permissions(path, Permissions::from_mode(0o600))` and the same on `bak_path` while it briefly exists. Alternatively, gate before serialize by setting umask 0o077 around the `gpu.index.serialize` call (the cache.rs:295-420 SEC-V1.33-2 pattern). Same fix needed for `cagra.rs:1453` (`write_meta_atomic` `File::create` for the JSON sidecar — `id_map` field at line 877 leaks every chunk_id, which embeds filenames + line ranges).

#### PL-V1.38-2: SPLADE `splade.index.bin.bak` inherits umask on Windows + leaks 0o600 on cross-device fallback
- **Difficulty:** easy
- **Location:** `src/splade/index.rs:483` (`std::fs::rename(path, &bak_path)`), `src/fs.rs:55` (`atomic_replace` cross-device `std::fs::copy` fallback)
- **Description:** Two related gaps in the new `.bak` rollback path landed in #1491. (1) The live `splade.index.bin` is correctly born `0o600` on Unix via `OpenOptionsExt::mode(0o600)` at line 391, but on Windows the `cfg(not(unix))` branch at line 396 falls back to `File::create` with no ACL hardening — same shape as the audit-mode finding (P4-19), but for SPLADE which is a new site (#1491). (2) When the live file is renamed to `.bak` at line 483 and then `atomic_replace` (`fs.rs:51-85`) hits a cross-device error (WSL `/mnt/c/`, NFS, overlayfs), the fallback path uses `std::fs::copy(tmp_path, &dest_tmp)` which on Windows does NOT preserve the source ACL (CopyFileW skips DACL by default), and on Unix only preserves source mode if `tmp_path` actually has restrictive perms — the doc-comment at fs.rs:27-29 promises "the rename preserves them on unix" but the cross-device branch silently breaks that contract. So a hardened `.tmp` ends up with default ACL on Windows after the EXDEV fallback fires.
- **Suggested fix:** (1) Add a Windows ACL fixup branch at splade/index.rs:394 mirroring whatever the umbrella P4-19 fix lands on. (2) In `fs.rs::atomic_replace` cross-device branch at line 55, after `std::fs::copy`, re-apply the source's permissions explicitly (`std::fs::set_permissions(&dest_tmp, std::fs::metadata(tmp_path)?.permissions())`); update the doc-comment at line 27-29 to be honest about the Windows-cross-device case.

#### PL-V1.38-3: `is_suspicious_cache_path` doc-comment promises Windows checks the impl skips
- **Difficulty:** easy
- **Location:** `src/aux_model.rs:115-117` (doc) vs `src/aux_model.rs:139-150` (impl)
- **Description:** Doc-comment at line 115-117 says "World-writable or guest-shared dirs: `/tmp`, `/var/tmp`, `/dev/shm` (Linux); `%TEMP%`, `%TMP%` (Windows)." The implementation only checks the three Linux paths as hardcoded string literals. On Windows, a hostile `HF_HOME=C:\Windows\Temp\hf` or `HF_HOME=C:\Users\Public\hf` flies straight through `is_suspicious_cache_path` and gets returned to the embedder loader — the SEC-V1.33-8 / #1339 supply-chain protection is silently no-op on Windows. Same shape as the existing "docs lying is P1" rule. Also misses `std::env::temp_dir()` on every platform (a custom `$TMPDIR=/var/run/...` setup on Linux escapes the check the same way).
- **Suggested fix:** After the hardcoded prefix loop at line 139-150, `if let Some(t) = std::env::temp_dir().to_str() { if path.starts_with(t) { return Some("under platform temp_dir"); } }`. On `cfg(windows)`, also check `std::env::var_os("PUBLIC")` (typically `C:\Users\Public`) and `std::env::var_os("ProgramData")`. Or rewrite the doc to match what the code does.

#### PL-V1.38-4: Two divergent WSL-DrvFS detectors disagree on custom `automount.root`
- **Difficulty:** medium
- **Location:** `src/config.rs:107-124` (`is_wsl_drvfs_path`) vs `src/cli/watch/mod.rs:289-294` (`is_under_wsl_automount` + cached parse of `/etc/wsl.conf`)
- **Description:** The watch helper reads `/etc/wsl.conf` for `[automount] root=/win/` and returns true for `/win/c/...`, correctly triggering `--poll`. But `coarse_fs_resolution` (config.rs:157-162) calls `is_wsl_drvfs_path` which is hard-coded to `/mnt/<letter>/` + `//wsl.localhost/` + `//wsl$/` — it never reads wsl.conf. Result: on a WSL host with `automount.root=/win/` and a project at `/win/c/Projects/foo`, the watch loop polls (good) but `coarse_fs_resolution` returns 0 (Linux ext4-equivalent) instead of 2s — and `events.rs::collect_events` mtime-skip uses strict-equality on a 2s-granular FS, silently dropping every rapid re-save. The bug class PB-V1.30.1-5 / #1225 was supposed to close. Same data living in two places diverged when only one helper learned about wsl.conf.
- **Suggested fix:** Promote `parse_wsl_automount_root` from cli/watch/mod.rs to `cqs::config` (or a new `cqs::wsl` module) and have `is_wsl_drvfs_path` consult it via the same `OnceLock`. Both call sites then share a single source of truth. Keep the `is_wsl_drvfs_path` signature (it stays a `&Path` predicate) so the watch detector becomes a thin wrapper.

#### PL-V1.38-5: `cqs init` writes a `.gitignore` missing 11 of the 14 files `.cqs/` actually contains
- **Difficulty:** easy
- **Location:** `src/cli/commands/infra/init.rs:44-47`
- **Description:** The init helper writes a 9-entry gitignore (`index.db`, `index.db-wal`, `index.db-shm`, `index.lock`, `index.hnsw.{graph,data,ids,checksum,lock}`, `*.tmp`). A real cqs project rooted in this repo's `.cqs/` actually has: `audit-mode.json`, `embeddings_cache.db`, `index.cagra`, `index.cagra.meta`, `index.cagra.bak`, `index_base.hnsw.*`, `splade.index.bin`, `splade.index.bin.bak`, `slots/`, `slots.lock`, `store.db`, `telemetry*.jsonl`, `telemetry.lock`, `active_slot` — **none** are gitignored. A user runs `cqs init` then `git add .` and commits ~hundreds of MB of binary index data, including `audit-mode.json` (deliberately marked SEC-1) and `telemetry*.jsonl` (operator hostnames, exec timing). Cross-platform issue. Slot/CAGRA/SPLADE/cache/audit/telemetry files were all added after the gitignore template was authored and never updated.
- **Suggested fix:** Replace the literal string with a file-list source-of-truth. Either (a) generate the gitignore from a `const SLICE: &[&str] = &[...]` that other code can also iterate, or (b) write `*\n!.gitignore\n` (gitignore-everything-but-itself) — this is the simpler solution and survives all future additions. The CRLF-vs-LF split at lines 44-47 also flaps on macOS users with `core.autocrlf=true`; gate on `is_wsl()` or document why only Windows-native gets CRLF.

#### PL-V1.38-6: `daemon_control_hint` returns systemctl on linux + pkill on macOS — silently wrong on FreeBSD/OpenBSD/illumos
- **Difficulty:** easy
- **Location:** `src/cli/commands/infra/model.rs:75-112`
- **Description:** The three-arm cfg matches `linux`, `macos`, and a generic `not(any(linux, macos))` fallback. The fallback returns "stop the cqs watch process" prose, which is correct *prose* but technically wrong on FreeBSD/OpenBSD/NetBSD/illumos — `daemon_translate.rs` and the watch socket path are `cfg(unix)`, so the daemon DOES build and run on these systems, and the operator gets a useless hint instead of `pkill -TERM -f 'cqs watch --serve'` which works identically to the macOS branch. Low-priority because cqs's officially-supported targets are Linux/macOS/Windows per CHANGELOG, but the README says "POSIX-compatible" — and the fallback contradicts that.
- **Suggested fix:** Change the gates from `cfg(target_os = "linux")` / `cfg(target_os = "macos")` / `cfg(not(...))` to `cfg(target_os = "linux")` / `cfg(unix)` / `cfg(not(unix))`. The macOS branch's `pkill` invocation works on every BSD and illumos. Same fix in `stop_daemon_best_effort` at line 649-704.

#### PL-V1.38-7: SPLADE / CAGRA tmp-file `to_string_lossy()` fallback can collide across concurrent saves on non-UTF-8 paths
- **Difficulty:** medium
- **Location:** `src/splade/index.rs:369-376`, `src/cagra.rs:1310-1315` and `src/cagra.rs:1445-1450`
- **Description:** All three sites build a tmp filename from `path.file_name().to_string_lossy()` and join with `temp_suffix()` to disambiguate. The doc-comment at splade/index.rs:365-368 specifically calls out the `to_str().unwrap_or(default)` collision risk and switches to `to_string_lossy()` — but `to_string_lossy()` ALSO collapses on non-UTF-8 input, replacing every invalid byte with `U+FFFD` (`\u{FFFD}`). Two concurrent saves to `splade.index_\xff_a.bin` and `splade.index_\xff_b.bin` get the same `to_string_lossy` output and only `temp_suffix()` (a 64-bit random) saves us — collision probability is low but nonzero, and the random `temp_suffix` fact isn't documented in the splade comment which talks only about `to_string_lossy`. CAGRA has the worse shape: line 1313 falls back to `"index.cagra"` and line 1448 falls back to `"cagra_meta"` on non-UTF-8 — both shared across all concurrent saves of any non-UTF-8 path. On Linux/macOS, file names CAN legally be non-UTF-8 (NTFS-mounted volumes via WSL, FUSE, archived/extracted tarballs from non-UTF-8 locales). Test fixtures only ever use ASCII, so this never trips locally.
- **Suggested fix:** Build the tmp basename from `as_encoded_bytes()` + hex-encode (or `OsStr` round-trip), guaranteeing a unique 1:1 mapping per source path. Or — cheaper — use `temp_suffix()` ALONE without the `file_name`-based prefix; the temp file is in the same dir as the live file and gets renamed away within microseconds, the prefix is mostly cosmetic.

#### PL-V1.38-8: `train_data::git::Command::new("git")` has the same PATH-lookup gap PB-V1.33-10 fixed for `tasklist`
- **Difficulty:** medium
- **Location:** `src/train_data/git.rs:77, 167, 316, 389, 419, 428, 434, 445, 451, 473, 479` and `src/train_data/mod.rs:537`
- **Description:** Every `train_data` invocation does `Command::new("git")` and relies on PATH lookup. PB-V1.33-10 / #1463 / #1490 just landed the fix for `tasklist` (resolve absolute path from `%SystemRoot%\System32`) precisely because a stripped-PATH context (Docker, GHA Windows runner with custom PATH, systemd unit with `Environment=PATH=...`) silently fails with `ErrorKind::NotFound`. The `train_data` flow is CLI-only and the operator running `cqs train ...` likely has git on PATH today, but the cqs daemon could (in a future Windows port — #1512) trigger train-data work, and a service-account daemon often has a stripped PATH. Same root cause; not a runtime crash but a silent "no commits found" that the operator can't diagnose.
- **Suggested fix:** Either (a) cache a one-time `which::which("git").context("git not on PATH; required for train-data extraction")?` at process start and pass `&Path` through; or (b) add a `resolve_git_path()` helper mirroring `cli/files.rs::process_exists` Windows shape — `%ProgramFiles%\Git\bin\git.exe` on Windows, `/usr/bin/git` then `/usr/local/bin/git` on Unix. (b) is more in keeping with the PB-V1.33-10 pattern. Lower priority than the cli/files.rs `tasklist` case because `git` on PATH is reasonable to assume in most train-data contexts.

#### PL-V1.38-9: `coarse_fs_resolution` returns Duration::ZERO on Windows — FAT32/exFAT mounts silently drop rapid saves
- **Difficulty:** medium
- **Location:** `src/config.rs:157-185` (function body), `src/config.rs:173-181` (Windows `else` branch)
- **Description:** `coarse_fs_resolution` returns 2s for WSL DrvFS, calls `linux_fs_resolution` / `macos_fs_resolution` to detect FAT/HFS/SMB/NFS via statfs magic numbers, but on Windows native (the `cfg(not(any(linux, macos)))` branch at 173-181) returns `None` → `Duration::ZERO`. That's correct for NTFS (100ns granularity) but Windows users CAN have a project on a USB FAT32 drive, an SD card with exFAT, or a network SMB share — all of which have 2s mtime granularity. The watch loop's mtime-equality skip then silently drops every rapid second-save, exactly the bug class PB-V1.30.1-5 / #1225 was meant to close on Linux/macOS. cqs runs on Windows per CHANGELOG; native Windows isn't in the v1.36.2 audit's PB scope but this is a fresh "narrow cfg gate" pattern.
- **Suggested fix:** Add a `windows_fs_resolution` shim that calls `GetVolumeInformationW` on the path's volume root and reads `lpFileSystemNameBuffer`; map "FAT" / "FAT32" / "exFAT" / "CDFS" / "UDF" to 2s, "NTFS" to 0. The `windows-sys` crate already pulls in the necessary bindings (used elsewhere). Or punt and return 1s globally on Windows — slight overshoot but correct on every FS the user could mount.

#### PL-V1.38-10: `aux_model::is_path_like` accepts paths but `dirs::cache_dir()` differs Windows-vs-WSL — same string is "cached" or "outside home" depending on cqs.exe vs cqs (Linux)
- **Difficulty:** medium
- **Location:** `src/aux_model.rs:163-168` (`in_cache = dirs::cache_dir().is_some_and(...)`)
- **Description:** `is_suspicious_cache_path` flags paths "outside user's home + system cache dir". On WSL Linux, `dirs::cache_dir()` returns `~/.cache/` (Linux XDG); on Windows-native it returns `%LOCALAPPDATA%`; on WSL with `wsl --windows-host` interop, neither helps for paths like `/mnt/c/Users/foo/AppData/Local/`. Result: the same `HF_HOME=/mnt/c/Users/foo/AppData/Local/huggingface` flagged "outside home + cache" by WSL cqs (Linux build) is ACCEPTED by Windows cqs.exe — same path, same operator, opposite outcomes. Operator confusion + a real protection gap when running cqs across platforms on shared paths. Same path-handling skew as the existing `lookup_main_cqs_dir` finding (audit-findings.md:693) but in the SEC-V1.33-8 supply-chain check.
- **Suggested fix:** When the path lives under a WSL-known Windows mount (`is_wsl_drvfs_path`), translate to the Windows-side cache dir for the comparison: `/mnt/c/Users/foo/AppData/Local` → equivalent of `%LOCALAPPDATA%`. Or document that `CQS_HF_CACHE_TRUSTED=1` is required when crossing the WSL/Windows boundary. The doc-comment at line 121-123 already mentions `%LOCALAPPDATA%` — make the impl honor it under WSL.

---

### Summary

10 platform findings, all confirming patterns already audited in v1.33 / v1.36.2 but at NEW
sites added in the v1.36-v1.38 window. The dominant class is "new persistence path landed
without the SEC-1 0o600/ACL discipline that older paths enforce" (PL-V1.38-1, -2, -5).
Two findings are docs-vs-code lies (PL-V1.38-3, -10 — `is_suspicious_cache_path` claims
Windows coverage it never had). Two are duplicate-source-of-truth divergences that broke
when only one copy got updated (PL-V1.38-4 WSL detector, PL-V1.38-5 gitignore template).
PL-V1.38-1 (CAGRA chmod) and PL-V1.38-5 (gitignore) are the highest-impact: a default
`cqs init && git add .` commits the `audit-mode.json` SEC-1 file, and CAGRA's loose mode
contradicts the SECURITY.md promise on multi-user Linux. The rest are latent and tracked
under the existing P4-9 / #1512 Windows-port umbrella, surfaced here so the umbrella PR
covers them in one pass instead of leaving "PL-V1.39-* one more place to fix" for later.
