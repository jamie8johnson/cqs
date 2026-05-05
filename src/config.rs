//! Configuration file support for cqs
//!
//! Config files are loaded in order (later overrides earlier):
//! 1. `~/.config/cqs/config.toml` (user defaults)
//! 2. `.cqs.toml` in project root (project overrides)
//!
//! CLI flags override all config file values.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Cap on config-file reads. Used by `Config::load_file` (stat-then-read)
/// and by the locked read-modify-write paths (`add_reference_to_config` /
/// `remove_reference_from_config`) which take the cap at the I/O layer
/// via `Read::take`. Real `cqs` config files are typically a few KB; 1
/// MiB is several orders of magnitude above realistic content.
const MAX_CONFIG_SIZE: u64 = 1024 * 1024;

/// Typed error for config file operations (EH-15).
/// Used by `add_reference_to_config` and `remove_reference_from_config`.
/// CLI callers convert to `anyhow::Error` at the boundary via the blanket `From`.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("TOML parse error: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("TOML serialize error: {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error("Duplicate reference: {0}")]
    DuplicateReference(String),
    #[error("Invalid config format: {0}")]
    InvalidFormat(String),
}

/// Detect if running under Windows Subsystem for Linux (cached).
///
/// PB-V1.29-10: `/proc/version` alone is not conclusive — Mariner Linux,
/// some Azure images, and custom kernels contain the substring
/// "microsoft" or "wsl" without being a WSL guest. Requiring a second
/// positive signal prevents those hosts from silently taking WSL-only
/// code paths (DrvFS permission-check skips, debounce bumps, etc.).
///
/// Returns true iff **any** of:
/// 1. `WSL_DISTRO_NAME` env var is set (WSL always sets this)
/// 2. `/proc/sys/fs/binfmt_misc/WSLInterop` exists (kernel-registered
///    interop entry; only present on real WSL)
/// 3. `/proc/version` matches `microsoft`/`wsl` AND `WSL_INTEROP` env is
///    set (the `WSL_INTEROP` env var points at the WSL interop socket,
///    so its presence is a strong second signal)
#[cfg(unix)]
pub fn is_wsl() -> bool {
    static IS_WSL: OnceLock<bool> = OnceLock::new();
    *IS_WSL.get_or_init(|| {
        // Signal 1: WSL_DISTRO_NAME is set by WSL itself.
        if std::env::var_os("WSL_DISTRO_NAME").is_some() {
            return true;
        }
        // Signal 2: binfmt_misc WSLInterop entry is kernel-registered only
        // on real WSL distros.
        if Path::new("/proc/sys/fs/binfmt_misc/WSLInterop").exists() {
            return true;
        }
        // Signal 3: /proc/version substring match REQUIRES a second env-var
        // corroboration (`WSL_INTEROP`). Neither is sufficient on its own:
        // /proc/version can match on Mariner/Azure, and `WSL_INTEROP` could
        // theoretically be user-set. The AND keeps the false-positive rate
        // near zero.
        let proc_version_matches = std::fs::read_to_string("/proc/version")
            .map(|v| {
                let lower = v.to_lowercase();
                lower.contains("microsoft") || lower.contains("wsl")
            })
            .unwrap_or(false);
        proc_version_matches && std::env::var_os("WSL_INTEROP").is_some()
    })
}

/// Non-Unix platforms are never WSL
#[cfg(not(unix))]
pub fn is_wsl() -> bool {
    false
}

/// Check whether a path lives under a WSL DrvFS automount
/// (`/mnt/<letter>/...`) or a UNC path that reaches into WSL
/// (`//wsl.localhost/...`, `//wsl$/...`), where advisory file locking is
/// unreliable and NTFS reports permission bits as `0o777`.
///
/// PB-V1.29-6: Consolidates the three inline `"/mnt/"` prefix checks
/// (`hnsw/persist.rs`, `project.rs`, and the per-path permission gate in
/// this file) into a single helper. The `/mnt/<letter>/` pattern avoids
/// false-positives on plain Linux hosts that legitimately mount
/// filesystems below `/mnt/` (e.g. `/mnt/data/` on a native Linux server
/// was being treated as WSL DrvFS by the naive prefix check).
///
/// P3.36: accept uppercase drive letters too — WSL with
/// `automount.options=case=force` exposes paths as `/mnt/C/...` — and
/// recognise the Windows-side UNC entry points `//wsl.localhost/<distro>/`
/// and the legacy `//wsl$/<distro>/`.
///
/// Returns `false` for non-UTF8 paths (WSL DrvFS paths are always UTF-8
/// under the Linux view) and for anything that doesn't match one of those
/// three shapes.
pub fn is_wsl_drvfs_path(path: &Path) -> bool {
    let s = match path.to_str() {
        Some(s) => s,
        None => return false,
    };
    // /mnt/<letter>/  — accept upper or lowercase drive letter.
    if let Some(rest) = s.strip_prefix("/mnt/") {
        let bytes = rest.as_bytes();
        if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b'/' {
            return true;
        }
    }
    // UNC paths reaching back into WSL from Windows-side tools.
    if s.starts_with("//wsl.localhost/") || s.starts_with("//wsl$/") {
        return true;
    }
    false
}

/// Returns the mtime resolution (granularity) of the filesystem holding
/// `path`. Files written within this window can collide on identical
/// stored mtimes — the watch loop's mtime-equality skip
/// (`events.rs::collect_events`) must treat any cached mtime within this
/// window as ambiguous and let the reindex through, otherwise the second
/// rapid save silently doesn't make it into the index.
///
/// PB-V1.30.1-5 / #1225: prior shape only handled WSL drvfs by toggling
/// `<` vs `<=` against the cached mtime. That misses HFS+, SMB, NFS, and
/// FAT32 mounts on plain Linux/macOS, all of which round mtime to ≥1 s.
/// The new function returns a `Duration` so the caller can compare
/// `now - cached <= resolution` uniformly across platforms.
///
/// Resolution by FS:
/// - WSL drvfs (`/mnt/<letter>/`, `//wsl$/...`): **2 s**
///   (NTFS via 9P bridge in practice; safer to overshoot).
/// - Linux NFS / CIFS / SMB / VFAT / FAT32 / MSDOS / HFS+: **2 s**
///   (Detected via `statfs::f_type` magic numbers.)
/// - macOS HFS+ / SMB / AFP / NFS / MS-DOS: **2 s**
///   (Detected via `statfs::f_fstypename` string.)
/// - Everything else (ext4, APFS, btrfs, xfs, zfs, tmpfs): **0**
///
/// 2 s is conservative: the worst-case granularity in this list is
/// FAT32's 2-second floor, and a uniform constant simplifies the call
/// site. The cost of an overshoot is at most one redundant reindex on
/// rapid re-saves; the cost of an undershoot is silent missed reindexes,
/// which is the bug class this issue closes.
///
/// Returns `Duration::ZERO` on stat failure (treat as fine-grained — the
/// caller's `<=` comparison degenerates to strict-equality skip, which is
/// the historical behavior on unknown mounts).
pub fn coarse_fs_resolution(path: &Path) -> std::time::Duration {
    use std::time::Duration;

    if is_wsl_drvfs_path(path) {
        return Duration::from_secs(2);
    }

    // One per-platform `let` so the function has a single tail
    // expression — clippy's `needless_return` lint kicks at every
    // cfg-gated `return` otherwise.
    #[cfg(target_os = "linux")]
    let resolution = linux_fs_resolution(path);

    #[cfg(target_os = "macos")]
    let resolution = macos_fs_resolution(path);

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let resolution: Option<Duration> = {
        let _ = path;
        None
    };

    resolution.unwrap_or(Duration::ZERO)
}

/// Linux: read `statfs::f_type` and map well-known coarse-mtime magic
/// numbers to a 2 s resolution. Magic constants follow `<linux/magic.h>`.
///
/// Returns `None` on stat failure or unknown FS — caller treats as
/// fine-grained (Duration::ZERO).
#[cfg(target_os = "linux")]
fn linux_fs_resolution(path: &Path) -> Option<std::time::Duration> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::time::Duration;

    let cpath = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statfs(cpath.as_ptr(), &mut stat) };
    if rc != 0 {
        return None;
    }

    // f_type is `__fsword_t` (signed `long` on most architectures). Cast to
    // i64 to compare against constants written in their natural unsigned
    // form. The CIFS / SMB2 magic numbers are 32-bit values that have the
    // top bit set (0xff534d42 / 0xfe534d42); writing them as `u32 as i64`
    // preserves the bit pattern across architectures where i32 vs i64
    // sign-extension would otherwise differ.
    const NFS_SUPER_MAGIC: i64 = 0x6969;
    const MSDOS_SUPER_MAGIC: i64 = 0x4d44;
    const SMB_SUPER_MAGIC: i64 = 0x517b;
    const HFS_PLUS_MAGIC: i64 = 0x482b;
    const VFAT_SUPER_MAGIC: i64 = 0x4d44; // alias for MSDOS family
    let cifs_magic: i64 = 0xff534d42_u32 as i64;
    let smb2_magic: i64 = 0xfe534d42_u32 as i64;

    let f_type = stat.f_type as i64;
    if f_type == NFS_SUPER_MAGIC
        || f_type == MSDOS_SUPER_MAGIC
        || f_type == SMB_SUPER_MAGIC
        || f_type == HFS_PLUS_MAGIC
        || f_type == VFAT_SUPER_MAGIC
        || f_type == cifs_magic
        || f_type == smb2_magic
    {
        return Some(Duration::from_secs(2));
    }
    Some(Duration::ZERO)
}

/// macOS: read `statfs::f_fstypename` and map known coarse-mtime FS
/// names to a 2 s resolution. APFS keeps nanosecond mtime, so it returns
/// `Duration::ZERO` along with all unknown filesystems.
#[cfg(target_os = "macos")]
fn macos_fs_resolution(path: &Path) -> Option<std::time::Duration> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::time::Duration;

    let cpath = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statfs(cpath.as_ptr(), &mut stat) };
    if rc != 0 {
        return None;
    }

    // f_fstypename is a fixed-size [c_char; MFSTYPENAMELEN] (16). Take a
    // null-terminated CStr view, lossy-decode, and match against the known
    // coarse-mtime names. This avoids depending on libc constants that
    // have changed between bindings versions.
    let name_bytes: Vec<u8> = stat
        .f_fstypename
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    let name = String::from_utf8_lossy(&name_bytes).to_ascii_lowercase();
    if matches!(
        name.as_str(),
        "hfs" | "smbfs" | "afpfs" | "nfs" | "msdos" | "exfat" | "cifs"
    ) {
        return Some(Duration::from_secs(2));
    }
    Some(Duration::ZERO)
}

/// Reference index configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReferenceConfig {
    /// Display name (used in results, CLI commands)
    pub name: String,
    /// Directory containing index.db + HNSW files
    pub path: PathBuf,
    /// Original source directory (for `ref update`)
    pub source: Option<PathBuf>,
    /// Score multiplier (0.0-1.0, default 0.8)
    #[serde(default = "default_ref_weight")]
    pub weight: f32,
}

/// Returns the default reference weight used for normalization calculations.
/// # Returns
/// A floating-point value of 0.8 representing the standard reference weight.
fn default_ref_weight() -> f32 {
    0.8
}

/// Auxiliary model configuration block (shared shape for SPLADE + reranker).
///
/// Parsed from `[splade]` / `[reranker]` sections of `.cqs.toml`. A preset
/// name resolves through [`crate::aux_model::preset`]; an explicit
/// `model_path` overrides the preset. `tokenizer_path` is inferred from
/// `model_path.parent().join("tokenizer.json")` when omitted, matching the
/// on-disk convention where both files live side-by-side.
///
/// Leave all fields unset to keep the hardcoded defaults:
/// `ensembledistil` for `[splade]`, `ms-marco-minilm` for `[reranker]`.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct AuxModelSection {
    /// Preset name. Looked up in the shared registry
    /// ([`crate::aux_model::preset`]) when set. Ignored if `model_path`
    /// is also set — explicit paths always win.
    pub preset: Option<String>,
    /// Explicit path to `model.onnx`. Beats `preset` when both are set.
    pub model_path: Option<PathBuf>,
    /// Explicit path to `tokenizer.json`. Inferred from `model_path`'s
    /// parent when omitted; rejected when set without `model_path`.
    pub tokenizer_path: Option<PathBuf>,
}

/// Optional overrides for search scoring parameters.
///
/// Loaded from the `[scoring]` section of `.cqs.toml` or
/// `~/.config/cqs/config.toml`. Every key under that section is collected
/// into [`ScoringOverrides::knobs`] and matched against
/// [`crate::search::scoring::knob::SCORING_KNOBS`] at resolve time —
/// adding a new scoring knob is one row in that table, no field churn here.
///
/// Known knob names (full set in [`crate::search::scoring::knob`]): `rrf_k`,
/// `type_boost`, `name_exact`, `name_contains`, `name_contained_by`,
/// `name_max_overlap`, `note_boost_factor`, `importance_test`,
/// `importance_private`, `parent_boost_per_child`, `parent_boost_cap`.
/// Unknown keys are logged at WARN; out-of-range values are clamped at
/// load time using each knob's `[min, max]`.
///
/// # TOML constraint
///
/// `#[serde(flatten)]` over a `HashMap<String, f32>` requires every key
/// under `[scoring]` to deserialize as an `f32` — nested tables (e.g.
/// `[scoring.advanced]`) would fail to parse. None exist today; if a
/// non-`f32` knob is added later, switch to a typed wrapper.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct ScoringOverrides {
    /// Flat map of knob name → value, populated from every key under
    /// `[scoring]` in the config file. Use [`Self::get`] to read named
    /// knobs without indexing into the map directly.
    #[serde(flatten)]
    pub knobs: HashMap<String, f32>,
}

impl ScoringOverrides {
    /// Look up an override by knob name. Returns `None` if the knob
    /// was not set in the config file.
    pub fn get(&self, name: &str) -> Option<f32> {
        self.knobs.get(name).copied()
    }
}

/// Configuration options loaded from config files
/// # Example
/// ```toml
/// # ~/.config/cqs/config.toml or .cqs.toml
/// limit = 10          # Default result limit
/// threshold = 0.3     # Minimum similarity score
/// name_boost = 0.2    # Weight for name matching
/// quiet = false       # Suppress progress output
/// verbose = false     # Enable verbose logging
/// stale_check = false # Disable per-file staleness checks
/// [[reference]]
/// name = "tokio"
/// path = "/home/user/.local/share/cqs/refs/tokio"
/// source = "/home/user/code/tokio"
/// weight = 0.8
/// ```
#[derive(Default, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Default result limit (overridden by -n)
    pub limit: Option<usize>,
    /// Default similarity threshold (overridden by -t)
    pub threshold: Option<f32>,
    /// Default name boost for hybrid search (overridden by --name-boost)
    pub name_boost: Option<f32>,
    /// Enable quiet mode by default
    pub quiet: Option<bool>,
    /// Enable verbose mode by default
    pub verbose: Option<bool>,
    /// Disable staleness checks (useful on NFS or slow filesystems)
    pub stale_check: Option<bool>,
    /// HNSW search width (higher = more accurate but slower, default 100)
    pub ef_search: Option<usize>,
    /// LLM model name (overridden by CQS_LLM_MODEL env var)
    pub llm_model: Option<String>,
    /// LLM API base URL (overridden by CQS_API_BASE env var)
    pub llm_api_base: Option<String>,
    /// LLM max tokens for summary generation (overridden by CQS_LLM_MAX_TOKENS env var)
    pub llm_max_tokens: Option<u32>,
    /// LLM max tokens for HyDE query predictions (overridden by CQS_HYDE_MAX_TOKENS env var)
    pub llm_hyde_max_tokens: Option<u32>,
    /// Embedding model configuration
    #[serde(default)]
    pub embedding: Option<crate::embedder::EmbeddingConfig>,
    /// Reranker model repository (overridden by CQS_RERANKER_MODEL env var)
    pub reranker_model: Option<String>,
    /// Reranker max input length in tokens (overridden by CQS_RERANKER_MAX_LENGTH env var)
    pub reranker_max_length: Option<usize>,
    /// Scoring parameter overrides (optional `[scoring]` section)
    #[serde(default)]
    pub scoring: Option<ScoringOverrides>,
    /// SPLADE sparse encoder configuration (optional `[splade]` section).
    /// Unset → hardcoded `ensembledistil` default.
    #[serde(default)]
    pub splade: Option<AuxModelSection>,
    /// Cross-encoder reranker configuration (optional `[reranker]` section).
    /// Unset → hardcoded `ms-marco-minilm` default. The legacy top-level
    /// `reranker_model` / `reranker_max_length` fields remain for backward
    /// TOML compatibility but are not consumed by the resolver.
    #[serde(default)]
    pub reranker: Option<AuxModelSection>,
    /// Reference indexes for multi-index search
    #[serde(default, rename = "reference")]
    pub references: Vec<ReferenceConfig>,
    /// Index-pipeline configuration (`[index]` section).
    /// Currently exposes only `vendored_paths` (#1221) — extend here if
    /// future index-time knobs land.
    #[serde(default)]
    pub index: Option<IndexConfig>,
}

/// `[index]` section of `.cqs.toml`. Drives index-pipeline behaviour
/// that doesn't fit cleanly under the existing top-level fields.
///
/// Currently a single knob (#1221): override the vendored-path prefix
/// list used to flag chunks for the `trust_level: "vendored-code"`
/// downgrade. `None` (the field absent in TOML) → cqs's default list
/// (`vendor`, `node_modules`, `third_party`, `.cargo`, `target`,
/// `dist`, `build`); `Some(empty)` → vendored detection disabled;
/// `Some(non_empty)` → use exactly those segment names.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IndexConfig {
    /// Override list of bare directory-segment names that flag a chunk
    /// as vendored at index time. Match is path-segment-based: an entry
    /// `"vendor"` matches `vendor/foo.rs` and `nested/vendor/bar.rs`
    /// but not `myvendor/baz.rs`. See `crate::vendored` for the
    /// matching algorithm + default list.
    #[serde(default)]
    pub vendored_paths: Option<Vec<String>>,
}

/// SEC-3: Redact a URL for logging — masks credentials (user:pass@host) and
/// returns only the scheme + host. Returns "[redacted]" for unparseable URLs.
fn redact_url(url: &str) -> String {
    // Strip credentials if present (scheme://user:pass@host/path -> scheme://host/path)
    if let Some(scheme_end) = url.find("://") {
        let after_scheme = &url[scheme_end + 3..];
        let host_part = if let Some(at_pos) = after_scheme.find('@') {
            &after_scheme[at_pos + 1..]
        } else {
            after_scheme
        };
        // Keep only scheme + host (strip path)
        let host_only = host_part.split('/').next().unwrap_or(host_part);
        format!("{}://{}/...", &url[..scheme_end], host_only)
    } else {
        "[redacted]".to_string()
    }
}

/// Custom Debug impl for Config that redacts llm_api_base to avoid logging credentials.
impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("limit", &self.limit)
            .field("threshold", &self.threshold)
            .field("name_boost", &self.name_boost)
            .field("quiet", &self.quiet)
            .field("verbose", &self.verbose)
            .field("stale_check", &self.stale_check)
            .field("ef_search", &self.ef_search)
            .field("llm_model", &self.llm_model)
            .field(
                "llm_api_base",
                &self.llm_api_base.as_deref().map(redact_url),
            )
            .field("llm_max_tokens", &self.llm_max_tokens)
            .field("llm_hyde_max_tokens", &self.llm_hyde_max_tokens)
            .field("embedding", &self.embedding)
            .field("reranker_model", &self.reranker_model)
            .field("reranker_max_length", &self.reranker_max_length)
            .field("scoring", &self.scoring)
            .field("splade", &self.splade)
            .field("reranker", &self.reranker)
            .field("references", &self.references)
            .finish()
    }
}

/// Clamp f32 config value to valid range and warn if out of bounds.
/// TC-48: Also catches NaN (which silently passes all comparisons as false)
/// and clamps it to `min`, preventing silent data loss in downstream filters.
/// SHL-V1.30-6: Resolve `CQS_MAX_REFERENCES` (default 20).
///
/// Memoized via `OnceLock` so the env-var read happens once at first
/// `validate()` call rather than on every load. The value is documented in
/// `README.md`'s env-var table. Each reference is ~50-100 MB, so the default
/// keeps a worst-case load under ~1-2 GB; bump on machines that can afford
/// it. Zero / non-numeric values fall back to the default.
fn max_references() -> usize {
    static CACHE: OnceLock<usize> = OnceLock::new();
    *CACHE.get_or_init(|| {
        const DEFAULT: usize = 20;
        std::env::var("CQS_MAX_REFERENCES")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT)
    })
}

fn clamp_config_f32(value: &mut f32, name: &str, min: f32, max: f32) {
    if value.is_nan() {
        tracing::warn!(field = name, "Config value is NaN, clamping to min");
        *value = min;
        return;
    }
    if *value < min || *value > max {
        tracing::warn!(
            field = name,
            value = *value,
            min,
            max,
            "Config value out of bounds, clamping"
        );
        *value = value.clamp(min, max);
    }
}

/// Clamp usize config value to valid range and warn if out of bounds
fn clamp_config_usize(value: &mut usize, name: &str, min: usize, max: usize) {
    if *value < min || *value > max {
        tracing::warn!(
            field = name,
            value = *value,
            min,
            max,
            "Config value out of bounds, clamping"
        );
        *value = (*value).clamp(min, max);
    }
}

impl Config {
    /// Load configuration from user and project config files
    pub fn load(project_root: &Path) -> Self {
        let user_config = dirs::config_dir()
            .map(|d| d.join("cqs/config.toml"))
            .and_then(|p| match Self::load_file(&p) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to load config file");
                    None
                }
            })
            .unwrap_or_default();

        let project_config = match Self::load_file(&project_root.join(".cqs.toml")) {
            Ok(c) => c.unwrap_or_default(),
            Err(e) => {
                tracing::warn!(error = %e, "Failed to load config file");
                Config::default()
            }
        };

        // Project overrides user
        let mut merged = user_config.override_with(project_config);
        merged.validate();

        tracing::debug!(?merged, "Effective config");
        merged
    }

    /// Clamp all fields to valid ranges and enforce invariants.
    /// Called once from `load()` after merging user + project configs.
    /// Adding a new field? Add its clamping here — this is the single
    /// validation choke point.
    fn validate(&mut self) {
        // SHL-28 / SHL-V1.30-6: Cap reference count. Each reference opens a separate
        // SQLite DB + HNSW index, consuming ~50-100MB RAM. 20 references = ~1-2GB
        // baseline memory. If you need more, consider consolidating related libraries
        // into fewer indexes — or override via `CQS_MAX_REFERENCES`.
        let max_references = max_references();
        if self.references.len() > max_references {
            // OB-V1.36-2: tracing::warn! only — daemons (cqs watch / serve)
            // have no TTY, eprintln! either lands as unstructured stderr in
            // journald or vanishes. Matches the OB-V1.30.1-9 contract used
            // elsewhere; CLI runs still surface this via the default subscriber.
            tracing::warn!(
                count = self.references.len(),
                max = max_references,
                "Too many references configured, truncating; \
                 each reference consumes ~50-100MB RAM"
            );
            self.references.truncate(max_references);
        }

        // Clamp reference weights to [0.0, 1.0]
        for r in &mut self.references {
            clamp_config_f32(&mut r.weight, "reference.weight", 0.0, 1.0);
        }

        // SEC-4 + SEC-NEW-1: Warn if reference `path` OR `source` is outside
        // project and home directories. SEC-4 covered `r.path` only; SEC-NEW-1
        // (v1.22.0 audit) found that `r.source` was never validated, so a
        // malicious checked-in `.cqs.toml` with `source = "/home/user/.ssh"`
        // would cause `cqs ref update` to index arbitrary files into the
        // reference DB (data exfiltration).
        // PB-V1.33-1: use `dunce::canonicalize` and canonicalize both
        // sides of the comparison so Windows verbatim (`\\?\C:\...`) vs
        // non-verbatim differences don't trip the SEC-4/SEC-NEW-1 warn.
        let home = dirs::home_dir().and_then(|h| dunce::canonicalize(h).ok());
        let cwd = std::env::current_dir()
            .ok()
            .and_then(|c| dunce::canonicalize(c).ok());
        for r in &self.references {
            // Check both path and source (if present)
            let paths_to_check: Vec<(&str, &std::path::Path)> = {
                let mut v = vec![("path", r.path.as_path())];
                if let Some(ref src) = r.source {
                    v.push(("source", src.as_path()));
                }
                v
            };
            for (field, p) in paths_to_check {
                // EH-V1.33-9: silent `if let Ok(...) = canonicalize(p)` swallowed
                // canonicalize failures (typo'd path, ENOENT, EACCES) and skipped the
                // SEC-4 / SEC-NEW-1 audit entirely — the protection meant to flag a
                // malicious `.cqs.toml` was effectively opt-out by error. Fail-loud:
                // a canonicalize error means we can't audit, so warn explicitly and
                // treat as untrusted.
                match dunce::canonicalize(p) {
                    Ok(canonical) => {
                        let in_home = home.as_ref().is_some_and(|h| canonical.starts_with(h));
                        let in_project = cwd.as_ref().is_some_and(|p| canonical.starts_with(p));
                        let in_cqs_dir = canonical.components().any(|c| c.as_os_str() == ".cqs");
                        if !in_home && !in_project && !in_cqs_dir {
                            tracing::warn!(
                                name = %r.name,
                                field,
                                path = %canonical.display(),
                                "Reference {field} is outside project and home directories — \
                                 a malicious .cqs.toml could use this to index arbitrary files. \
                                 Verify the source is intentional."
                            );
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            name = %r.name,
                            field,
                            path = %p.display(),
                            error = %e,
                            "Cannot canonicalize reference {field} for SEC-4 audit; \
                             treating as untrusted. A typo in `.cqs.toml` or a path the \
                             user cannot access bypasses the in-home/in-project check; \
                             verify the path is correct and reachable."
                        );
                    }
                }
            }
        }
        if let Some(ref mut limit) = self.limit {
            clamp_config_usize(limit, "limit", 1, 100);
        }
        if let Some(ref mut t) = self.threshold {
            clamp_config_f32(t, "threshold", 0.0, 1.0);
        }
        if let Some(ref mut nb) = self.name_boost {
            clamp_config_f32(nb, "name_boost", 0.0, 1.0);
        }
        if let Some(ref mut ef) = self.ef_search {
            clamp_config_usize(ef, "ef_search", 10, 1000);
        }
        // SHL-26: Models like Claude support up to 64k output tokens; 4096 was too restrictive.
        if let Some(ref mut mt) = self.llm_max_tokens {
            if *mt == 0 || *mt > 32768 {
                tracing::warn!(
                    field = "llm_max_tokens",
                    value = *mt,
                    "Config value out of bounds, clamping to [1, 32768]"
                );
                *mt = (*mt).clamp(1, 32768);
            }
        }
        if let Some(ref mut s) = self.scoring {
            // Clamp known knobs to their [min, max]; warn + drop unknown keys.
            // Each knob's bounds live in `SCORING_KNOBS` — adding a new knob
            // doesn't change anything here.
            let known: std::collections::HashSet<&'static str> =
                crate::search::scoring::knob::SCORING_KNOBS
                    .iter()
                    .map(|k| k.name)
                    .collect();
            s.knobs.retain(|key, _| {
                if known.contains(key.as_str()) {
                    true
                } else {
                    tracing::warn!(
                        key = %key,
                        "Unknown key in [scoring] config — dropping (no such knob)"
                    );
                    false
                }
            });
            for k in crate::search::scoring::knob::SCORING_KNOBS.iter() {
                if let Some(v) = s.knobs.get_mut(k.name) {
                    let label = format!("scoring.{}", k.name);
                    clamp_config_f32(v, &label, k.min, k.max);
                }
            }
        }
    }

    /// Load configuration from a specific file
    fn load_file(path: &Path) -> Result<Option<Self>, String> {
        // Size guard: config files should be well under 1MB.
        // Module-level `MAX_CONFIG_SIZE` is also used by the locked
        // read-modify-write paths.
        if let Ok(meta) = std::fs::metadata(path) {
            if meta.len() > MAX_CONFIG_SIZE {
                return Err(format!(
                    "Config file too large: {}KB (limit {}KB)",
                    meta.len() / 1024,
                    MAX_CONFIG_SIZE / 1024
                ));
            }
        }
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(format!("Failed to read config {}: {}", path.display(), e));
            }
        };

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Skip permission check on WSL (NTFS always reports 777) or Windows drive mounts.
            // PB-V1.29-6: Shared `is_wsl_drvfs_path` keeps the `/mnt/[a-z]/`
            // logic in one spot rather than duplicating it in every call site.
            let is_wsl_mount = is_wsl() || is_wsl_drvfs_path(path);
            if !is_wsl_mount {
                if let Ok(meta) = std::fs::metadata(path) {
                    let mode = meta.permissions().mode();
                    if mode & 0o077 != 0 {
                        tracing::warn!(
                            path = %path.display(),
                            mode = format!("{:o}", mode & 0o777),
                            "Config file is accessible by other users. Consider: chmod 600 {}",
                            path.display()
                        );
                    }
                }
            }
        }

        match toml::from_str::<Self>(&content) {
            Ok(config) => {
                tracing::debug!(path = %path.display(), ?config, "Loaded config");
                Ok(Some(config))
            }
            Err(e) => Err(format!("Failed to parse config {}: {}", path.display(), e)),
        }
    }

    /// Layer another config on top (other overrides self where present)
    fn override_with(self, other: Self) -> Self {
        // Merge references: project refs replace user refs by name, append new ones
        let mut refs = self.references;
        for proj_ref in other.references {
            if let Some(pos) = refs.iter().position(|r| r.name == proj_ref.name) {
                tracing::warn!(
                    name = proj_ref.name,
                    "Project config overrides user reference '{}'",
                    proj_ref.name
                );
                refs[pos] = proj_ref;
            } else {
                refs.push(proj_ref);
            }
        }

        // MERGE: add new Option<T> fields here (other.field.or(self.field))
        Config {
            limit: other.limit.or(self.limit),
            threshold: other.threshold.or(self.threshold),
            name_boost: other.name_boost.or(self.name_boost),
            quiet: other.quiet.or(self.quiet),
            verbose: other.verbose.or(self.verbose),
            stale_check: other.stale_check.or(self.stale_check),
            ef_search: other.ef_search.or(self.ef_search),
            llm_model: other.llm_model.or(self.llm_model),
            llm_api_base: other.llm_api_base.or(self.llm_api_base),
            llm_max_tokens: other.llm_max_tokens.or(self.llm_max_tokens),
            llm_hyde_max_tokens: other.llm_hyde_max_tokens.or(self.llm_hyde_max_tokens),
            embedding: other.embedding.or(self.embedding),
            reranker_model: other.reranker_model.or(self.reranker_model),
            reranker_max_length: other.reranker_max_length.or(self.reranker_max_length),
            scoring: other.scoring.or(self.scoring),
            splade: other.splade.or(self.splade),
            reranker: other.reranker.or(self.reranker),
            references: refs,
            index: other.index.or(self.index),
        }
    }
}

/// Add a reference to a config file (read-modify-write, preserves unknown fields)
pub fn add_reference_to_config(
    config_path: &Path,
    ref_config: &ReferenceConfig,
) -> Result<(), ConfigError> {
    // Acquire exclusive lock for the entire read-modify-write cycle.
    // Read through the locked fd to avoid TOCTOU between lock and read.
    //
    // NOTE: File locking is advisory only on WSL over 9P (DrvFs/NTFS mounts).
    // This prevents concurrent cqs processes from corrupting the config,
    // but cannot protect against external Windows process modifications.
    let lock_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(config_path)?;
    lock_file.lock()?;

    // Bounded read via `Read::take` so a hostile `<MAX_CONFIG_SIZE>+1`-byte
    // config can't OOM us before the size check fires (RM-V1.33-1). The
    // cap is enforced at the I/O layer, eliminating the stat→read TOCTOU
    // window that a plain `metadata()`-then-`read_to_string` would have.
    let mut content = String::new();
    use std::io::Read;
    (&lock_file)
        .take(MAX_CONFIG_SIZE + 1)
        .read_to_string(&mut content)?;
    if content.len() as u64 > MAX_CONFIG_SIZE {
        return Err(ConfigError::InvalidFormat(format!(
            "Config file too large: > {}KB (limit {}KB)",
            MAX_CONFIG_SIZE / 1024,
            MAX_CONFIG_SIZE / 1024
        )));
    }
    let mut table: toml::Table = if content.is_empty() {
        toml::Table::new()
    } else {
        content.parse()?
    };

    // Check for duplicate name
    if let Some(toml::Value::Array(arr)) = table.get("reference") {
        let has_duplicate = arr.iter().any(|v| {
            v.get("name")
                .and_then(|n| n.as_str())
                .map(|n| n == ref_config.name)
                .unwrap_or(false)
        });
        if has_duplicate {
            return Err(ConfigError::DuplicateReference(format!(
                "Reference '{}' already exists in {}",
                ref_config.name,
                config_path.display()
            )));
        }
    }

    let ref_value = toml::Value::try_from(ref_config)?;

    let refs = table
        .entry("reference")
        .or_insert_with(|| toml::Value::Array(vec![]));

    match refs {
        toml::Value::Array(arr) => arr.push(ref_value),
        _ => {
            return Err(ConfigError::InvalidFormat(
                "'reference' in config is not an array".to_string(),
            ))
        }
    }

    // Atomic write: temp file + rename (while holding lock).
    //
    // RM-V1.33-8: the write block is wrapped in a closure so the tmp
    // file is always cleaned up on any intermediate write/permission
    // failure. The previous code propagated `?` directly out of the
    // OpenOptions::open / write_all / std::fs::write calls, leaving
    // `<config>.toml.<hex>.tmp` files behind on disk-full / EIO. Names
    // include 16 hex chars of randomness so failures accumulated
    // distinct tmp files rather than overwriting a single one.
    let suffix = crate::temp_suffix();
    let tmp_path = config_path.with_extension(format!("toml.{:016x}.tmp", suffix));
    let serialized = toml::to_string_pretty(&table)?;
    let write_result: Result<(), ConfigError> = (|| {
        // SEC-1: Write with mode 0o600 from creation so file is never world-readable
        #[cfg(unix)]
        {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp_path)?;
            f.write_all(serialized.as_bytes())?;
        }
        #[cfg(not(unix))]
        {
            std::fs::write(&tmp_path, &serialized)?;
        }
        Ok(())
    })();
    if let Err(e) = write_result {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(e);
    }

    // atomic_replace: fsync tmp, rename with EXDEV fallback, fsync parent dir.
    // SEC-1: the tmp file was opened with mode(0o600), which fs::copy
    // preserves into the xdev fallback destination.
    crate::fs::atomic_replace(&tmp_path, config_path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp_path);
        ConfigError::Io(e)
    })?;

    // lock_file dropped here, releasing exclusive lock
    Ok(())
}

/// Remove a reference from a config file by name (read-modify-write)
pub fn remove_reference_from_config(config_path: &Path, name: &str) -> Result<bool, ConfigError> {
    // Acquire exclusive lock for the entire read-modify-write cycle.
    // Read through the locked fd to avoid TOCTOU between lock and read.
    //
    // NOTE: File locking is advisory only on WSL over 9P (DrvFs/NTFS mounts).
    // This prevents concurrent cqs processes from corrupting the config,
    // but cannot protect against external Windows process modifications.
    let lock_file = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(config_path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(ConfigError::Io(e)),
    };
    lock_file.lock()?;

    // Bounded read via `Read::take` — see add_reference_to_config above
    // (RM-V1.33-1).
    let mut content = String::new();
    use std::io::Read;
    (&lock_file)
        .take(MAX_CONFIG_SIZE + 1)
        .read_to_string(&mut content)?;
    if content.len() as u64 > MAX_CONFIG_SIZE {
        return Err(ConfigError::InvalidFormat(format!(
            "Config file too large: > {}KB (limit {}KB)",
            MAX_CONFIG_SIZE / 1024,
            MAX_CONFIG_SIZE / 1024
        )));
    }

    let mut table: toml::Table = content.parse()?;

    let removed = if let Some(toml::Value::Array(arr)) = table.get_mut("reference") {
        let before = arr.len();
        arr.retain(|v| {
            v.get("name")
                .and_then(|n| n.as_str())
                .map(|n| n != name)
                .unwrap_or(true)
        });
        let removed = arr.len() < before;
        // Clean up empty array
        if arr.is_empty() {
            table.remove("reference");
        }
        removed
    } else {
        false
    };

    if removed {
        // Atomic write: temp file + rename (while holding lock).
        // RM-V1.33-8: same closure-wrapped cleanup as
        // `add_reference_to_config` — see that site for rationale.
        let suffix = crate::temp_suffix();
        let tmp_path = config_path.with_extension(format!("toml.{:016x}.tmp", suffix));
        let serialized = toml::to_string_pretty(&table)?;
        let write_result: Result<(), ConfigError> = (|| {
            // SEC-1: Write with mode 0o600 from creation so file is never world-readable
            #[cfg(unix)]
            {
                use std::io::Write;
                use std::os::unix::fs::OpenOptionsExt;
                let mut f = std::fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .mode(0o600)
                    .open(&tmp_path)?;
                f.write_all(serialized.as_bytes())?;
            }
            #[cfg(not(unix))]
            {
                std::fs::write(&tmp_path, &serialized)?;
            }
            Ok(())
        })();
        if let Err(e) = write_result {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(e);
        }

        // atomic_replace: fsync tmp, rename with EXDEV fallback, fsync parent dir.
        crate::fs::atomic_replace(&tmp_path, config_path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp_path);
            ConfigError::Io(e)
        })?;
    }
    // lock_file dropped here, releasing exclusive lock
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_load_valid_config() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join(".cqs.toml");
        std::fs::write(&config_path, "limit = 10\nthreshold = 0.5\n").unwrap();

        let config = Config::load_file(&config_path).unwrap().unwrap();
        assert_eq!(config.limit, Some(10));
        assert_eq!(config.threshold, Some(0.5));
    }

    #[test]
    fn test_load_missing_file() {
        let dir = TempDir::new().unwrap();
        let config = Config::load_file(&dir.path().join("nonexistent.toml"));
        assert!(config.unwrap().is_none());
    }

    #[test]
    fn test_load_malformed_toml() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join(".cqs.toml");
        std::fs::write(&config_path, "not valid [[[").unwrap();

        let config = Config::load_file(&config_path);
        assert!(config.is_err());
    }

    #[test]
    fn test_merge_override() {
        let base = Config {
            limit: Some(10),
            threshold: Some(0.5),
            ..Default::default()
        };
        let override_cfg = Config {
            limit: Some(20),
            name_boost: Some(0.3),
            ..Default::default()
        };

        let merged = base.override_with(override_cfg);
        assert_eq!(merged.limit, Some(20));
        assert_eq!(merged.threshold, Some(0.5));
        assert_eq!(merged.name_boost, Some(0.3));
    }

    #[test]
    fn test_parse_config_with_references() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join(".cqs.toml");
        std::fs::write(
            &config_path,
            r#"
limit = 5

[[reference]]
name = "tokio"
path = "/home/user/.local/share/cqs/refs/tokio"
source = "/home/user/code/tokio"
weight = 0.8

[[reference]]
name = "serde"
path = "/home/user/.local/share/cqs/refs/serde"
"#,
        )
        .unwrap();

        let config = Config::load_file(&config_path).unwrap().unwrap();
        assert_eq!(config.limit, Some(5));
        assert_eq!(config.references.len(), 2);
        assert_eq!(config.references[0].name, "tokio");
        assert_eq!(config.references[0].weight, 0.8);
        assert!(config.references[0].source.is_some());
        assert_eq!(config.references[1].name, "serde");
        assert_eq!(config.references[1].weight, 0.8); // default
        assert!(config.references[1].source.is_none());
    }

    #[test]
    fn test_merge_references_replace_by_name() {
        let user = Config {
            references: vec![
                ReferenceConfig {
                    name: "tokio".into(),
                    path: "/old/path".into(),
                    source: None,
                    weight: 0.5,
                },
                ReferenceConfig {
                    name: "serde".into(),
                    path: "/serde/path".into(),
                    source: None,
                    weight: 0.8,
                },
            ],
            ..Default::default()
        };
        let project = Config {
            references: vec![
                ReferenceConfig {
                    name: "tokio".into(),
                    path: "/new/path".into(),
                    source: Some("/src/tokio".into()),
                    weight: 0.9,
                },
                ReferenceConfig {
                    name: "axum".into(),
                    path: "/axum/path".into(),
                    source: None,
                    weight: 0.7,
                },
            ],
            ..Default::default()
        };

        let merged = user.override_with(project);
        assert_eq!(merged.references.len(), 3);
        // tokio replaced
        assert_eq!(merged.references[0].name, "tokio");
        assert_eq!(merged.references[0].path, PathBuf::from("/new/path"));
        assert_eq!(merged.references[0].weight, 0.9);
        // serde kept
        assert_eq!(merged.references[1].name, "serde");
        // axum appended
        assert_eq!(merged.references[2].name, "axum");
    }

    #[test]
    fn test_add_reference_to_config_new_file() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join(".cqs.toml");

        let ref_config = ReferenceConfig {
            name: "tokio".into(),
            path: "/refs/tokio".into(),
            source: Some("/src/tokio".into()),
            weight: 0.8,
        };
        add_reference_to_config(&config_path, &ref_config).unwrap();

        let config = Config::load_file(&config_path).unwrap().unwrap();
        assert_eq!(config.references.len(), 1);
        assert_eq!(config.references[0].name, "tokio");
    }

    #[test]
    fn test_add_reference_to_config_preserves_fields() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join(".cqs.toml");
        std::fs::write(&config_path, "limit = 10\nthreshold = 0.5\n").unwrap();

        let ref_config = ReferenceConfig {
            name: "tokio".into(),
            path: "/refs/tokio".into(),
            source: None,
            weight: 0.8,
        };
        add_reference_to_config(&config_path, &ref_config).unwrap();

        let config = Config::load_file(&config_path).unwrap().unwrap();
        assert_eq!(config.limit, Some(10));
        assert_eq!(config.threshold, Some(0.5));
        assert_eq!(config.references.len(), 1);
    }

    #[test]
    fn test_add_reference_to_config_appends() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join(".cqs.toml");

        let ref1 = ReferenceConfig {
            name: "tokio".into(),
            path: "/refs/tokio".into(),
            source: None,
            weight: 0.8,
        };
        let ref2 = ReferenceConfig {
            name: "serde".into(),
            path: "/refs/serde".into(),
            source: None,
            weight: 0.7,
        };
        add_reference_to_config(&config_path, &ref1).unwrap();
        add_reference_to_config(&config_path, &ref2).unwrap();

        let config = Config::load_file(&config_path).unwrap().unwrap();
        assert_eq!(config.references.len(), 2);
        assert_eq!(config.references[0].name, "tokio");
        assert_eq!(config.references[1].name, "serde");
    }

    #[test]
    fn test_remove_reference_from_config() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join(".cqs.toml");

        let ref1 = ReferenceConfig {
            name: "tokio".into(),
            path: "/refs/tokio".into(),
            source: None,
            weight: 0.8,
        };
        let ref2 = ReferenceConfig {
            name: "serde".into(),
            path: "/refs/serde".into(),
            source: None,
            weight: 0.7,
        };
        add_reference_to_config(&config_path, &ref1).unwrap();
        add_reference_to_config(&config_path, &ref2).unwrap();

        let removed = remove_reference_from_config(&config_path, "tokio").unwrap();
        assert!(removed);

        let config = Config::load_file(&config_path).unwrap().unwrap();
        assert_eq!(config.references.len(), 1);
        assert_eq!(config.references[0].name, "serde");
    }

    #[test]
    fn test_remove_reference_not_found() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join(".cqs.toml");
        std::fs::write(&config_path, "limit = 5\n").unwrap();

        let removed = remove_reference_from_config(&config_path, "nonexistent").unwrap();
        assert!(!removed);
    }

    #[test]
    fn test_remove_reference_missing_file() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("nonexistent.toml");

        let removed = remove_reference_from_config(&config_path, "tokio").unwrap();
        assert!(!removed);
    }

    #[test]
    fn test_remove_last_reference_cleans_array() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join(".cqs.toml");

        let ref1 = ReferenceConfig {
            name: "tokio".into(),
            path: "/refs/tokio".into(),
            source: None,
            weight: 0.8,
        };
        add_reference_to_config(&config_path, &ref1).unwrap();
        remove_reference_from_config(&config_path, "tokio").unwrap();

        // Should still be valid config, just no references
        let config = Config::load_file(&config_path).unwrap().unwrap();
        assert!(config.references.is_empty());
    }

    #[test]
    fn test_add_reference_duplicate_name_errors() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join(".cqs.toml");

        let ref1 = ReferenceConfig {
            name: "tokio".into(),
            path: "/refs/tokio".into(),
            source: None,
            weight: 0.8,
        };
        add_reference_to_config(&config_path, &ref1).unwrap();

        // Adding same name again should fail
        let ref2 = ReferenceConfig {
            name: "tokio".into(),
            path: "/refs/tokio2".into(),
            source: None,
            weight: 0.5,
        };
        let result = add_reference_to_config(&config_path, &ref2);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already exists"));

        // Original should be unchanged
        let config = Config::load_file(&config_path).unwrap().unwrap();
        assert_eq!(config.references.len(), 1);
        assert_eq!(config.references[0].weight, 0.8);
    }

    #[test]
    fn test_weight_clamping() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join(".cqs.toml");

        // Write config with out-of-bounds weights
        std::fs::write(
            &config_path,
            r#"
[[reference]]
name = "over"
path = "/refs/over"
weight = 1.5

[[reference]]
name = "under"
path = "/refs/under"
weight = -0.5

[[reference]]
name = "valid"
path = "/refs/valid"
weight = 0.7
"#,
        )
        .unwrap();

        // Load config (should clamp weights)
        let config = Config::load(dir.path());

        // Find the references
        let over_ref = config.references.iter().find(|r| r.name == "over").unwrap();
        let under_ref = config
            .references
            .iter()
            .find(|r| r.name == "under")
            .unwrap();
        let valid_ref = config
            .references
            .iter()
            .find(|r| r.name == "valid")
            .unwrap();

        assert_eq!(
            over_ref.weight, 1.0,
            "Weight > 1.0 should be clamped to 1.0"
        );
        assert_eq!(
            under_ref.weight, 0.0,
            "Weight < 0.0 should be clamped to 0.0"
        );
        assert_eq!(
            valid_ref.weight, 0.7,
            "Valid weight should remain unchanged"
        );
    }

    #[test]
    fn test_threshold_clamping() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join(".cqs.toml");

        // Write config with out-of-bounds threshold
        std::fs::write(&config_path, "threshold = 1.5\n").unwrap();

        let config = Config::load(dir.path());
        assert_eq!(config.threshold, Some(1.0));
    }

    #[test]
    fn test_name_boost_clamping() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join(".cqs.toml");

        // Write config with out-of-bounds name_boost
        std::fs::write(&config_path, "name_boost = -0.1\n").unwrap();

        let config = Config::load(dir.path());
        assert_eq!(config.name_boost, Some(0.0));
    }

    #[test]
    fn test_limit_clamping_zero() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join(".cqs.toml");

        // Write config with limit=0
        std::fs::write(&config_path, "limit = 0\n").unwrap();

        let config = Config::load(dir.path());
        assert_eq!(config.limit, Some(1));
    }

    #[test]
    fn test_limit_clamping_large() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join(".cqs.toml");

        // Write config with limit=200
        std::fs::write(&config_path, "limit = 200\n").unwrap();

        let config = Config::load(dir.path());
        assert_eq!(config.limit, Some(100));
    }

    #[test]
    fn test_stale_check_config() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join(".cqs.toml");

        // stale_check = false disables staleness warnings
        std::fs::write(&config_path, "stale_check = false\n").unwrap();
        let config = Config::load(dir.path());
        assert_eq!(config.stale_check, Some(false));

        // stale_check = true (explicit enable, default behavior)
        std::fs::write(&config_path, "stale_check = true\n").unwrap();
        let config = Config::load(dir.path());
        assert_eq!(config.stale_check, Some(true));

        // Not set: defaults to None
        std::fs::write(&config_path, "limit = 5\n").unwrap();
        let config = Config::load(dir.path());
        assert_eq!(config.stale_check, None);
    }

    #[test]
    fn test_llm_config_fields() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join(".cqs.toml");
        std::fs::write(
            &config_path,
            r#"
llm_model = "claude-sonnet-4-20250514"
llm_api_base = "https://custom.api/v1"
llm_max_tokens = 200
"#,
        )
        .unwrap();

        let config = Config::load_file(&config_path).unwrap().unwrap();
        assert_eq!(
            config.llm_model.as_deref(),
            Some("claude-sonnet-4-20250514")
        );
        assert_eq!(
            config.llm_api_base.as_deref(),
            Some("https://custom.api/v1")
        );
        assert_eq!(config.llm_max_tokens, Some(200));
    }

    #[test]
    fn test_llm_max_tokens_clamping() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join(".cqs.toml");

        // Over max (cap is 32768)
        std::fs::write(&config_path, "llm_max_tokens = 99999\n").unwrap();
        let config = Config::load(dir.path());
        assert_eq!(config.llm_max_tokens, Some(32768));

        // Zero
        std::fs::write(&config_path, "llm_max_tokens = 0\n").unwrap();
        let config = Config::load(dir.path());
        assert_eq!(config.llm_max_tokens, Some(1));
    }

    #[test]
    fn test_llm_config_merge() {
        let base = Config {
            llm_model: Some("base-model".into()),
            llm_max_tokens: Some(100),
            ..Default::default()
        };
        let override_cfg = Config {
            llm_model: Some("override-model".into()),
            llm_api_base: Some("https://override/v1".into()),
            ..Default::default()
        };

        let merged = base.override_with(override_cfg);
        assert_eq!(merged.llm_model.as_deref(), Some("override-model"));
        assert_eq!(merged.llm_api_base.as_deref(), Some("https://override/v1"));
        assert_eq!(merged.llm_max_tokens, Some(100)); // from base, not overridden
    }

    #[test]
    fn test_embedding_config_preset() {
        let toml = r#"
        [embedding]
        model = "bge-large"
        "#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.embedding.as_ref().unwrap().model, "bge-large");
    }

    #[test]
    fn test_embedding_config_custom() {
        let toml = r#"
        [embedding]
        model = "custom"
        repo = "my-org/my-model"
        dim = 384
        "#;
        let config: Config = toml::from_str(toml).unwrap();
        let emb = config.embedding.as_ref().unwrap();
        assert_eq!(emb.model, "custom");
        assert_eq!(emb.dim, Some(384));
    }

    #[test]
    fn test_no_embedding_section() {
        let toml = "limit = 10\n";
        let config: Config = toml::from_str(toml).unwrap();
        assert!(config.embedding.is_none());
    }

    // ===== TC-36/TC-48: NaN threshold clamped to min =====

    #[test]
    fn tc36_nan_threshold_clamped_to_min() {
        // TC-48: NaN is now caught by clamp_config_f32 and clamped to min (0.0
        // for threshold). Previously NaN silently passed through because all NaN
        // comparisons return false.
        let mut config = Config {
            threshold: Some(f32::NAN),
            ..Default::default()
        };
        config.validate();
        // NaN is now caught and clamped to min (0.0 for threshold)
        assert_eq!(config.threshold, Some(0.0));
    }

    #[test]
    fn tc48_nan_name_boost_clamped_to_min() {
        let mut config = Config {
            name_boost: Some(f32::NAN),
            ..Default::default()
        };
        config.validate();
        assert_eq!(
            config.name_boost,
            Some(0.0),
            "NaN name_boost should be clamped to 0.0"
        );
    }

    // ===== TC-37: Edge case dimension metadata =====

    #[test]
    fn tc37_embedding_config_empty_string_model() {
        // Empty model name should fall back to default via from_preset returning None
        std::env::remove_var("CQS_EMBEDDING_MODEL");
        let embedding_cfg = crate::embedder::EmbeddingConfig {
            model: String::new(),
            ..Default::default()
        };
        let cfg = crate::embedder::ModelConfig::resolve(None, Some(&embedding_cfg));
        assert_eq!(
            cfg.name,
            crate::embedder::ModelConfig::default_model().name,
            "Empty model string should fall back to default"
        );
    }

    // ===== TC-39: embedding section tokenizer_path parsing =====

    #[test]
    fn tc39_embedding_tokenizer_path_parsed() {
        let toml = r#"
        [embedding]
        model = "custom"
        repo = "org/model"
        dim = 384
        tokenizer_path = "custom.json"
        "#;
        let config: Config = toml::from_str(toml).unwrap();
        let emb = config.embedding.as_ref().unwrap();
        assert_eq!(
            emb.tokenizer_path.as_deref(),
            Some("custom.json"),
            "tokenizer_path should be captured from config"
        );
    }

    #[test]
    fn tc39_embedding_unknown_field_ignored() {
        // Unknown fields like `tokenizer` (without `_path`) should be ignored by serde
        let toml = r#"
        [embedding]
        model = "e5-base"
        "#;
        let config: Config = toml::from_str(toml).unwrap();
        let emb = config.embedding.as_ref().unwrap();
        assert!(
            emb.tokenizer_path.is_none(),
            "tokenizer_path should be None when not specified"
        );
    }

    // ===== RX-2: ScoringOverrides config parsing =====

    #[test]
    fn test_scoring_overrides_parsed() {
        let toml = r#"
        [scoring]
        name_exact = 0.9
        note_boost_factor = 0.25
        "#;
        let config: Config = toml::from_str(toml).unwrap();
        let s = config.scoring.as_ref().unwrap();
        assert!((s.get("name_exact").unwrap() - 0.9).abs() < f32::EPSILON);
        assert!((s.get("note_boost_factor").unwrap() - 0.25).abs() < f32::EPSILON);
        assert!(s.get("name_contains").is_none());
    }

    #[test]
    fn test_scoring_overrides_absent() {
        let toml = "limit = 5\n";
        let config: Config = toml::from_str(toml).unwrap();
        assert!(config.scoring.is_none());
    }

    #[test]
    fn test_scoring_overrides_clamped() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join(".cqs.toml");
        std::fs::write(
            &config_path,
            "[scoring]\nname_exact = 5.0\nimportance_test = -1.0\n",
        )
        .unwrap();
        let config = Config::load(dir.path());
        let s = config.scoring.as_ref().unwrap();
        assert!(
            (s.get("name_exact").unwrap() - 2.0).abs() < f32::EPSILON,
            "name_exact clamped to 2.0"
        );
        assert!(
            (s.get("importance_test").unwrap() - 0.0).abs() < f32::EPSILON,
            "importance_test clamped to 0.0"
        );
    }

    #[test]
    fn test_scoring_overrides_drops_unknown_keys() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join(".cqs.toml");
        std::fs::write(&config_path, "[scoring]\nrrf_k = 80.0\nbogus_knob = 99.0\n").unwrap();
        let config = Config::load(dir.path());
        let s = config.scoring.as_ref().unwrap();
        assert_eq!(s.get("rrf_k"), Some(80.0));
        assert!(
            s.get("bogus_knob").is_none(),
            "unknown keys must be dropped at config load"
        );
    }

    // ===== [splade] / [reranker] section parsing =====

    #[test]
    fn test_splade_section_preset_only() {
        let toml = r#"
        [splade]
        preset = "splade-code-0.6b"
        "#;
        let config: Config = toml::from_str(toml).unwrap();
        let s = config.splade.as_ref().unwrap();
        assert_eq!(s.preset.as_deref(), Some("splade-code-0.6b"));
        assert!(s.model_path.is_none());
        assert!(s.tokenizer_path.is_none());
    }

    #[test]
    fn test_splade_section_explicit_paths() {
        let toml = r#"
        [splade]
        model_path = "/models/splade/model.onnx"
        tokenizer_path = "/models/splade/tokenizer.json"
        "#;
        let config: Config = toml::from_str(toml).unwrap();
        let s = config.splade.as_ref().unwrap();
        assert!(s.preset.is_none());
        assert_eq!(
            s.model_path.as_deref(),
            Some(Path::new("/models/splade/model.onnx"))
        );
        assert_eq!(
            s.tokenizer_path.as_deref(),
            Some(Path::new("/models/splade/tokenizer.json"))
        );
    }

    #[test]
    fn test_splade_section_model_path_without_tokenizer() {
        // Omitting tokenizer_path is fine — aux_model::resolve infers it
        // from model_path's parent.
        let toml = r#"
        [splade]
        model_path = "/models/splade/model.onnx"
        "#;
        let config: Config = toml::from_str(toml).unwrap();
        let s = config.splade.as_ref().unwrap();
        assert!(s.tokenizer_path.is_none());
    }

    #[test]
    fn test_reranker_section_preset() {
        let toml = r#"
        [reranker]
        preset = "ms-marco-minilm"
        "#;
        let config: Config = toml::from_str(toml).unwrap();
        let s = config.reranker.as_ref().unwrap();
        assert_eq!(s.preset.as_deref(), Some("ms-marco-minilm"));
    }

    #[test]
    fn test_no_splade_or_reranker_section() {
        let toml = "limit = 10\n";
        let config: Config = toml::from_str(toml).unwrap();
        assert!(config.splade.is_none());
        assert!(config.reranker.is_none());
    }

    #[test]
    fn test_splade_section_merge() {
        let base = Config {
            splade: Some(AuxModelSection {
                preset: Some("ensembledistil".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let over = Config {
            splade: Some(AuxModelSection {
                preset: Some("splade-code-0.6b".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let merged = base.override_with(over);
        assert_eq!(
            merged.splade.unwrap().preset.as_deref(),
            Some("splade-code-0.6b")
        );
    }

    #[test]
    fn test_scoring_overrides_merge() {
        let base = Config {
            scoring: Some(ScoringOverrides {
                knobs: [("name_exact".to_string(), 0.9_f32)].into_iter().collect(),
            }),
            ..Default::default()
        };
        let over = Config {
            scoring: Some(ScoringOverrides {
                knobs: [("note_boost_factor".to_string(), 0.3_f32)]
                    .into_iter()
                    .collect(),
            }),
            ..Default::default()
        };
        // Project overrides user — whole scoring section replaced
        let merged = base.override_with(over);
        let s = merged.scoring.unwrap();
        assert!((s.get("note_boost_factor").unwrap() - 0.3).abs() < f32::EPSILON);
        // base scoring was replaced, not field-merged
        assert!(s.get("name_exact").is_none());
    }

    /// PB-V1.30.1-5 / #1225: WSL drvfs paths report a 2 s coarse-mtime
    /// resolution regardless of the underlying NTFS/FAT32 — the 9P
    /// bridge's worst-case rounding is what matters at the watch-loop
    /// layer.
    #[test]
    fn coarse_fs_resolution_returns_two_seconds_for_wsl_drvfs() {
        use std::time::Duration;
        let two_sec = Duration::from_secs(2);
        assert_eq!(
            coarse_fs_resolution(Path::new("/mnt/c/Projects/foo")),
            two_sec
        );
        assert_eq!(coarse_fs_resolution(Path::new("/mnt/d/some/path")), two_sec);
        assert_eq!(coarse_fs_resolution(Path::new("/mnt/C/UpperCase")), two_sec);
        assert_eq!(
            coarse_fs_resolution(Path::new("//wsl.localhost/Ubuntu/home/user")),
            two_sec
        );
        assert_eq!(
            coarse_fs_resolution(Path::new("//wsl$/Ubuntu/home/user")),
            two_sec
        );
    }

    /// PB-V1.30.1-5 / #1225: paths under `/tmp` (tmpfs on Linux) and
    /// other native fine-grained filesystems must report
    /// `Duration::ZERO` so the `events.rs` mtime-equality skip stays in
    /// the historical fast path on the steady-state common case. Pinned
    /// using a TempDir which lands on the runner's tmpfs (Linux CI) or
    /// APFS (macOS CI), both fine-grained.
    #[test]
    fn coarse_fs_resolution_returns_zero_for_native_fine_grained_fs() {
        use std::time::Duration;
        let dir = tempfile::TempDir::new().unwrap();
        // tmpfs on Linux, APFS/HFS+ on macOS. CI runners are the main
        // target here; HFS+ would actually return 2 s under the new
        // `macos_fs_resolution`, but GitHub-hosted macOS runners have
        // been APFS-only since 2018, so this assertion holds in CI.
        // If a developer runs `cargo test` on an external HFS+ drive,
        // they'd see the 2 s return value — that's a feature, not a
        // bug.
        assert_eq!(coarse_fs_resolution(dir.path()), Duration::ZERO);
    }

    /// PB-V1.30.1-5 / #1225: stat failure returns `Duration::ZERO`
    /// (treat as fine-grained) — the caller's `<=` mtime check
    /// degenerates to the historical strict-equality skip. A
    /// nonexistent path is the cleanest stat-failure reproduction.
    #[test]
    fn coarse_fs_resolution_returns_zero_on_stat_failure() {
        use std::time::Duration;
        let nonexistent = Path::new("/nonexistent/cqs-test-path-that-must-not-exist-12345");
        assert_eq!(coarse_fs_resolution(nonexistent), Duration::ZERO);
    }

    /// PB-V1.30.1-5 / #1225: the `is_wsl_drvfs_path` shortcut takes
    /// precedence over the platform-specific statfs check — even when
    /// the underlying mount is reported as some unrelated FS magic,
    /// WSL drvfs always returns 2 s. Pinned with a synthetic path that
    /// matches the prefix without needing a real mount.
    #[test]
    fn coarse_fs_resolution_wsl_shortcut_does_not_call_statfs() {
        use std::time::Duration;
        let two_sec = Duration::from_secs(2);
        // Path doesn't exist on disk; statfs would fail. The shortcut
        // returns before the syscall happens, so we get 2 s anyway.
        assert_eq!(
            coarse_fs_resolution(Path::new("/mnt/c/this/path/does/not/exist")),
            two_sec
        );
    }
}
