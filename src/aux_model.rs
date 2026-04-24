//! Shared config + resolution for auxiliary models (SPLADE, reranker).
//!
//! Factored out of [`crate::splade`] and [`crate::reranker`] so both share a
//! single config surface, preset registry, and resolution precedence —
//! instead of each module duplicating the env-var / default dance.
//!
//! # Resolution precedence (highest wins)
//!
//! 1. **CLI override** — caller-supplied string. Treated as a filesystem path
//!    when it starts with `/` or `~/`, otherwise as an HF repo id. A repo id
//!    is only meaningful for the reranker (which fetches from the Hub);
//!    SPLADE always operates on a local directory.
//! 2. **Environment variable** — e.g. `CQS_SPLADE_MODEL`, `CQS_RERANKER_MODEL`.
//!    Same path-vs-repo semantics as the CLI override.
//! 3. **Explicit config paths** — `[splade] model_path = "..."` /
//!    `tokenizer_path = "..."` in `.cqs.toml`. Explicit beats preset.
//! 4. **Config preset name** — `[splade] preset = "splade-code-0.6b"`.
//!    Looked up in the preset registry ([`preset`]).
//! 5. **Hardcoded default preset** — final fallback when nothing is set
//!    (e.g. `"ensembledistil"` for SPLADE).
//!
//! The kind discriminant ([`AuxModelKind`]) selects both the preset table
//! and the on-disk filename conventions (SPLADE bundles live as
//! `model.onnx` + `tokenizer.json` in a directory; rerankers live as
//! `onnx/model.onnx` + `tokenizer.json` under a HF cross-encoder checkout).

use std::path::{Path, PathBuf};

/// Which auxiliary model is being resolved.
///
/// Selects the preset registry and the on-disk filename convention. SPLADE
/// expects a flat directory layout; reranker expects the HuggingFace
/// cross-encoder layout (`onnx/` subdirectory).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuxModelKind {
    /// SPLADE sparse encoder.
    ///
    /// On-disk layout: `{dir}/model.onnx` + `{dir}/tokenizer.json`.
    Splade,
    /// Cross-encoder reranker.
    ///
    /// On-disk layout: `{dir}/onnx/model.onnx` + `{dir}/tokenizer.json`
    /// (matches the HuggingFace cross-encoder repo layout, so a raw repo
    /// checkout "just works").
    Reranker,
}

/// Resolved auxiliary model configuration.
///
/// Holds either the concrete local file paths or the HF repo id that still
/// needs fetching. Callers are expected to check `repo.is_some()` to know
/// whether a Hub download is required — for SPLADE the paths are always
/// local, for the reranker they may be produced by `hf_hub` after this
/// resolution completes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuxModelConfig {
    /// Preset name that produced this config, if any. `None` when the config
    /// came from an explicit path override (CLI, env, or TOML `model_path`).
    /// Logged at load time so operators can tell which preset is live.
    pub preset: Option<String>,
    /// Path to the ONNX model file. For preset/repo configs that haven't
    /// been fetched yet, this is a synthetic path inside the expected
    /// download directory — callers fetching via HF Hub substitute the
    /// real downloaded path.
    pub model_path: PathBuf,
    /// Path to the `tokenizer.json`.
    pub tokenizer_path: PathBuf,
    /// HuggingFace repo id when the bundle should be fetched from the Hub.
    /// `None` for local-path configs. The reranker resolver consults this
    /// to decide whether to skip the HF API call.
    pub repo: Option<String>,
}

/// Errors surfaced by auxiliary model resolution.
///
/// Kept as a single enum so both [`crate::splade`] and [`crate::reranker`]
/// can convert to their own error types at the boundary without duplicating
/// variant shapes.
#[derive(Debug, thiserror::Error)]
pub enum AuxModelError {
    /// An explicit path was supplied (CLI, env, or TOML `model_path`) but
    /// it doesn't point at a valid bundle on disk.
    #[error("auxiliary model path not found: {0}")]
    NotFound(String),
    /// Config preset name didn't match any entry in the registry.
    #[error("unknown {kind:?} preset: {name}")]
    UnknownPreset { kind: AuxModelKind, name: String },
    /// Sanity check: both `model_path` and `preset` set at the TOML level is
    /// fine (path wins) but if a caller passes an inconsistent combination
    /// (e.g. `tokenizer_path` set without `model_path`) we reject rather
    /// than silently ignore.
    #[error("inconsistent config: {0}")]
    InvalidConfig(String),
}

/// Expand a leading `~/`, `~\`, or bare `~` against `$HOME`.
///
/// Returns the path unchanged when the input is absolute (without a tilde
/// prefix), relative, or `$HOME` can't be resolved. Mirrors the existing
/// expansion in `splade::resolve_splade_model_dir` so env-var semantics
/// stay identical.
///
/// PB-V1.29-9: Also accept bare `~` (which should resolve to `$HOME`)
/// and the Windows separator `~\` so a TOML config authored on Windows
/// survives the trip through `home_dir`.
fn expand_tilde(raw: &str) -> PathBuf {
    if raw == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from(raw));
    }
    if let Some(rest) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix(r"~\")) {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(raw)
}

/// Decide whether a user-supplied string looks like a filesystem path.
///
/// Paths start with `/` (absolute Unix), `~/` (home-relative), `\\` (UNC
/// share, e.g. `\\server\share\splade`), or any path that
/// [`Path::is_absolute`] recognizes (covers Windows drive-letter paths
/// like `C:\Models\splade` on Windows builds). Everything else is treated
/// as an HF repo id of the form `org/model`.
///
/// Deliberately conservative — we don't want to misinterpret
/// `./relative/path` as a repo id, but we also don't want to guess about
/// bare `foo/bar` which is a valid repo id.
fn is_path_like(raw: &str) -> bool {
    // PB-V1.29-9: also catch bare `~` and the Windows-separator form `~\`
    // so detection matches `expand_tilde`'s acceptance set.
    raw == "~"
        || raw.starts_with('/')
        || raw.starts_with("~/")
        || raw.starts_with(r"~\")
        || raw.starts_with("\\\\")
        || std::path::Path::new(raw).is_absolute()
}

/// EX-V1.29-9: On-disk layout template for an auxiliary model bundle.
///
/// The prior `config_from_dir(kind, ...)` hardcoded a layout per
/// `AuxModelKind`, which meant every new preset had to match one of the
/// two baked-in shapes (`model.onnx` at root for SPLADE, `onnx/model.onnx`
/// for reranker). Newer HF repos and custom exports don't always fit; a
/// preset now carries its own layout via [`AuxModelKind::default_layout`]
/// with room for per-preset overrides.
///
/// Paths are joined directly without separator canonicalization — callers
/// supply already-correct filenames for the target platform. Forward
/// slashes work on all supported platforms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirLayout {
    /// Filename of the ONNX model relative to the preset directory
    /// (e.g. `"model.onnx"` for SPLADE, `"onnx/model.onnx"` for reranker).
    pub onnx_rel_path: &'static str,
    /// Filename of the tokenizer JSON relative to the preset directory
    /// (e.g. `"tokenizer.json"`).
    pub tokenizer_rel_path: &'static str,
}

impl AuxModelKind {
    /// Default [`DirLayout`] for this kind, matching the HuggingFace
    /// convention historically baked into `config_from_dir`.
    fn default_layout(self) -> DirLayout {
        match self {
            AuxModelKind::Splade => DirLayout {
                onnx_rel_path: "model.onnx",
                tokenizer_rel_path: "tokenizer.json",
            },
            AuxModelKind::Reranker => DirLayout {
                onnx_rel_path: "onnx/model.onnx",
                tokenizer_rel_path: "tokenizer.json",
            },
        }
    }
}

/// Build an [`AuxModelConfig`] from a concrete directory, using the
/// layout supplied by the caller.
///
/// Used by both the explicit-path branches (CLI/env/TOML) and the preset
/// branch once the preset has been realized into a directory. The explicit-
/// path callers pass `kind.default_layout()`; preset callers may pass a
/// preset-specific layout when the HF repo deviates from the default shape.
fn config_from_dir(dir: &Path, layout: &DirLayout, preset: Option<String>) -> AuxModelConfig {
    AuxModelConfig {
        preset,
        model_path: dir.join(layout.onnx_rel_path),
        tokenizer_path: dir.join(layout.tokenizer_rel_path),
        repo: None,
    }
}

/// Preset registry — returns the config for a named preset of a given kind.
///
/// Returns `None` when the name isn't registered for that kind. Preset
/// entries resolve to repo ids (for reranker) or to a default local cache
/// directory (for SPLADE, where there's no HF-side SPLADE model we ship
/// out-of-the-box — operators download into `~/.cache/huggingface/...`).
///
/// # Shipped presets
///
/// * [`AuxModelKind::Splade`]:
///   - `"ensembledistil"` → `naver/splade-cocondenser-ensembledistil`
///     (current default, expected at `~/.cache/huggingface/splade-onnx`).
///   - `"splade-code-0.6b"` → `naver/splade-code-0.6B`
///     (expected at `~/.cache/huggingface/splade-code-0.6B`).
/// * [`AuxModelKind::Reranker`]:
///   - `"ms-marco-minilm"` → `cross-encoder/ms-marco-MiniLM-L-6-v2`
///     (current default).
pub fn preset(kind: AuxModelKind, name: &str) -> Option<AuxModelConfig> {
    let _span = tracing::debug_span!("aux_model_preset", ?kind, name).entered();
    match kind {
        AuxModelKind::Splade => splade_preset(name),
        AuxModelKind::Reranker => reranker_preset(name),
    }
}

/// SPLADE preset lookup. SPLADE bundles are loaded from a local directory
/// (the encoder never goes through HF Hub), so each preset points at an
/// expected cache path.
fn splade_preset(name: &str) -> Option<AuxModelConfig> {
    let home = dirs::home_dir()?;
    // EX-V1.29-9: presets carry an explicit layout. SPLADE's default is
    // currently the same for every shipped preset, but wiring the layout
    // through the preset registry means a future preset with a different
    // on-disk shape (e.g. `onnx/model.onnx` from a newer HF repo layout)
    // can override here without touching `config_from_dir`.
    let layout = AuxModelKind::Splade.default_layout();
    match name {
        "ensembledistil" | "splade-ensembledistil" => {
            let dir = home.join(".cache/huggingface/splade-onnx");
            Some(config_from_dir(
                &dir,
                &layout,
                Some("ensembledistil".into()),
            ))
        }
        "splade-code-0.6b" | "splade-code" => {
            let dir = home.join(".cache/huggingface/splade-code-0.6B");
            Some(config_from_dir(
                &dir,
                &layout,
                Some("splade-code-0.6b".into()),
            ))
        }
        _ => None,
    }
}

/// Reranker preset lookup. Reranker bundles default to HF Hub fetches, so
/// the preset produces a config with `repo = Some(...)` and synthetic paths
/// the Hub API rewrites later. If a concrete local dir was preferred,
/// operators set `[reranker] model_path = ...` instead of a preset.
fn reranker_preset(name: &str) -> Option<AuxModelConfig> {
    match name {
        "ms-marco-minilm" | "ms-marco-minilm-l-6" | "minilm" => {
            let repo = "cross-encoder/ms-marco-MiniLM-L-6-v2".to_string();
            Some(AuxModelConfig {
                preset: Some("ms-marco-minilm".into()),
                // model_path / tokenizer_path are placeholders — the HF fetch
                // path replaces them with the real downloaded file locations.
                model_path: PathBuf::from(&repo).join("onnx/model.onnx"),
                tokenizer_path: PathBuf::from(&repo).join("tokenizer.json"),
                repo: Some(repo),
            })
        }
        _ => None,
    }
}

/// Hardcoded default preset name for a kind. Used as the last fallback
/// when nothing else is configured.
pub fn default_preset_name(kind: AuxModelKind) -> &'static str {
    match kind {
        AuxModelKind::Splade => "ensembledistil",
        AuxModelKind::Reranker => "ms-marco-minilm",
    }
}

/// Resolve the final [`AuxModelConfig`] given the full precedence stack.
///
/// Precedence is documented at module level. This function does **not**
/// touch the filesystem for preset/repo configs — the caller (SPLADE
/// encoder loader / reranker HF fetch) decides whether the resolved
/// paths exist and whether to fall back. For explicit paths (CLI / env /
/// TOML `model_path`) we do verify the file exists at resolution time, so
/// obvious typos fail fast instead of producing a misleading "model
/// unavailable" warning from the consumer.
///
/// # Parameters
///
/// * `kind` — which auxiliary model to resolve for.
/// * `cli_override` — optional `--reranker-model` / `--splade-model`-style
///   flag value. Highest priority when `Some`.
/// * `env_var` — env var name to consult (e.g. `"CQS_SPLADE_MODEL"`).
///   Checked only when `cli_override` is unset.
/// * `config_preset` — `[splade] preset = "..."` / `[reranker] preset = "..."`
///   value from TOML.
/// * `config_path` — `[splade] model_path = "..."` / `[reranker] model_path = "..."`
///   value from TOML. Explicit path beats preset.
/// * `config_tokenizer_path` — `[splade] tokenizer_path = "..."` /
///   `[reranker] tokenizer_path = "..."`. Inferred from `config_path.parent().join("tokenizer.json")`
///   when `None` and `config_path` is `Some`.
/// * `default_preset` — hardcoded default preset name, used as the last
///   fallback. Pass [`default_preset_name`]`(kind)` in the normal case.
pub fn resolve(
    kind: AuxModelKind,
    cli_override: Option<&str>,
    env_var: &str,
    config_preset: Option<&str>,
    config_path: Option<&Path>,
    config_tokenizer_path: Option<&Path>,
    default_preset: &str,
) -> Result<AuxModelConfig, AuxModelError> {
    let _span = tracing::info_span!("aux_model_resolve", ?kind, env_var).entered();

    // 1. CLI override — path-or-repo.
    if let Some(raw) = cli_override.filter(|s| !s.is_empty()) {
        tracing::info!(source = "cli", value = %raw, "aux model resolved from CLI override");
        return resolve_raw(kind, raw);
    }

    // 2. Env var — path-or-repo.
    if let Ok(raw) = std::env::var(env_var) {
        if !raw.is_empty() {
            tracing::info!(
                source = env_var,
                value = %raw,
                "aux model resolved from environment variable"
            );
            return resolve_raw(kind, &raw);
        }
    }

    // 3. Explicit TOML path beats preset. `tokenizer_path` is inferred from
    //    `model_path.parent().join("tokenizer.json")` when omitted — matches
    //    the common case where both live in the same directory.
    if let Some(model_path) = config_path {
        let expanded = expand_tilde(&model_path.to_string_lossy());
        if !expanded.exists() {
            return Err(AuxModelError::NotFound(format!(
                "[{}] model_path = {} does not exist",
                section_name(kind),
                expanded.display()
            )));
        }
        let tokenizer_path = match config_tokenizer_path {
            Some(p) => {
                let expanded = expand_tilde(&p.to_string_lossy());
                if !expanded.exists() {
                    return Err(AuxModelError::NotFound(format!(
                        "[{}] tokenizer_path = {} does not exist",
                        section_name(kind),
                        expanded.display()
                    )));
                }
                expanded
            }
            None => expanded
                .parent()
                .unwrap_or(Path::new("."))
                .join("tokenizer.json"),
        };
        tracing::info!(
            source = "config",
            model_path = %expanded.display(),
            tokenizer_path = %tokenizer_path.display(),
            "aux model resolved from TOML model_path"
        );
        return Ok(AuxModelConfig {
            preset: None,
            model_path: expanded,
            tokenizer_path,
            repo: None,
        });
    }

    if config_tokenizer_path.is_some() {
        return Err(AuxModelError::InvalidConfig(format!(
            "[{}] tokenizer_path set without model_path",
            section_name(kind)
        )));
    }

    // 4. Config preset.
    if let Some(name) = config_preset.filter(|s| !s.is_empty()) {
        tracing::info!(
            source = "config_preset",
            name,
            "aux model resolved from TOML preset"
        );
        return preset(kind, name).ok_or_else(|| AuxModelError::UnknownPreset {
            kind,
            name: name.to_string(),
        });
    }

    // 5. Hardcoded default.
    tracing::debug!(default_preset, "aux model using hardcoded default preset");
    preset(kind, default_preset).ok_or_else(|| AuxModelError::UnknownPreset {
        kind,
        name: default_preset.to_string(),
    })
}

/// Resolve a raw path-or-repo string from CLI / env.
///
/// For the reranker, a string without a leading `/` or `~/` is treated as
/// an HF repo id and returned as a preset-less [`AuxModelConfig`] with
/// `repo = Some(raw)`. For SPLADE, which has no repo-fetching code path,
/// a bare identifier is rejected — operators must supply a real directory.
fn resolve_raw(kind: AuxModelKind, raw: &str) -> Result<AuxModelConfig, AuxModelError> {
    if is_path_like(raw) {
        let expanded = expand_tilde(raw);
        if !expanded.exists() {
            return Err(AuxModelError::NotFound(format!(
                "{} override {} does not exist",
                section_name(kind),
                expanded.display()
            )));
        }
        // EX-V1.29-9: explicit-path override uses the kind's default
        // layout. An operator supplying a custom repo path is expected to
        // lay it out in the canonical shape; if a future preset ships a
        // deviating layout it owns the call to `config_from_dir` itself.
        return Ok(config_from_dir(&expanded, &kind.default_layout(), None));
    }
    // Non-path-like input.
    match kind {
        AuxModelKind::Reranker => Ok(AuxModelConfig {
            preset: None,
            model_path: PathBuf::from(raw).join("onnx/model.onnx"),
            tokenizer_path: PathBuf::from(raw).join("tokenizer.json"),
            repo: Some(raw.to_string()),
        }),
        AuxModelKind::Splade => Err(AuxModelError::NotFound(format!(
            "splade override {raw} is not an absolute or home-relative path — \
             SPLADE does not fetch from HF Hub, supply a directory path"
        ))),
    }
}

/// TOML section name for a kind (used in error messages).
fn section_name(kind: AuxModelKind) -> &'static str {
    match kind {
        AuxModelKind::Splade => "splade",
        AuxModelKind::Reranker => "reranker",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialize all env-var-touching tests against a process-wide lock so
    /// they don't race against each other or against splade/reranker tests
    /// that set the same vars.
    static AUX_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// RAII guard that removes the given env var on drop. Ensures every
    /// test branch leaves the env clean even when an assertion panics.
    struct EnvGuard<'a> {
        key: &'a str,
    }
    impl Drop for EnvGuard<'_> {
        fn drop(&mut self) {
            std::env::remove_var(self.key);
        }
    }

    /// Write the canonical SPLADE bundle layout into `dir` so resolver
    /// path-existence checks are satisfied without a real ONNX graph.
    fn write_stub_splade_bundle(dir: &Path) {
        std::fs::write(dir.join("model.onnx"), b"stub").unwrap();
        std::fs::write(dir.join("tokenizer.json"), b"stub").unwrap();
    }

    #[test]
    fn test_preset_resolution_fallthrough() {
        // CLI None, env unset, config preset set → returns that preset's paths.
        let _lock = AUX_ENV_LOCK.lock().unwrap();
        std::env::remove_var("CQS_SPLADE_MODEL");
        let _g = EnvGuard {
            key: "CQS_SPLADE_MODEL",
        };

        let resolved = resolve(
            AuxModelKind::Splade,
            None,
            "CQS_SPLADE_MODEL",
            Some("splade-code-0.6b"),
            None,
            None,
            "ensembledistil",
        )
        .unwrap();
        assert_eq!(resolved.preset.as_deref(), Some("splade-code-0.6b"));
        // Paths point at the cache dir for the configured preset.
        let home = dirs::home_dir().unwrap();
        assert_eq!(
            resolved.model_path,
            home.join(".cache/huggingface/splade-code-0.6B/model.onnx")
        );
    }

    #[test]
    fn test_env_beats_config() {
        // Env set to a real path, config preset also set → env wins.
        let _lock = AUX_ENV_LOCK.lock().unwrap();
        let tmp = tempfile::TempDir::new().unwrap();
        write_stub_splade_bundle(tmp.path());

        std::env::set_var("CQS_SPLADE_MODEL", tmp.path());
        let _g = EnvGuard {
            key: "CQS_SPLADE_MODEL",
        };

        let resolved = resolve(
            AuxModelKind::Splade,
            None,
            "CQS_SPLADE_MODEL",
            Some("splade-code-0.6b"),
            None,
            None,
            "ensembledistil",
        )
        .unwrap();
        // Preset field cleared — this came from env, not a preset.
        assert_eq!(resolved.preset, None);
        assert_eq!(resolved.model_path, tmp.path().join("model.onnx"));
    }

    #[test]
    fn test_cli_beats_env() {
        // CLI and env both set → CLI wins.
        let _lock = AUX_ENV_LOCK.lock().unwrap();
        let cli_dir = tempfile::TempDir::new().unwrap();
        let env_dir = tempfile::TempDir::new().unwrap();
        write_stub_splade_bundle(cli_dir.path());
        write_stub_splade_bundle(env_dir.path());

        std::env::set_var("CQS_SPLADE_MODEL", env_dir.path());
        let _g = EnvGuard {
            key: "CQS_SPLADE_MODEL",
        };

        let resolved = resolve(
            AuxModelKind::Splade,
            Some(cli_dir.path().to_str().unwrap()),
            "CQS_SPLADE_MODEL",
            None,
            None,
            None,
            "ensembledistil",
        )
        .unwrap();
        // CLI path wins over env path.
        assert_eq!(resolved.model_path, cli_dir.path().join("model.onnx"));
    }

    #[test]
    fn test_absent_all_uses_hardcoded_default() {
        // Nothing set → hardcoded default preset resolves.
        let _lock = AUX_ENV_LOCK.lock().unwrap();
        std::env::remove_var("CQS_SPLADE_MODEL");
        let _g = EnvGuard {
            key: "CQS_SPLADE_MODEL",
        };

        let resolved = resolve(
            AuxModelKind::Splade,
            None,
            "CQS_SPLADE_MODEL",
            None,
            None,
            None,
            "ensembledistil",
        )
        .unwrap();
        assert_eq!(resolved.preset.as_deref(), Some("ensembledistil"));
        let home = dirs::home_dir().unwrap();
        assert_eq!(
            resolved.model_path,
            home.join(".cache/huggingface/splade-onnx/model.onnx")
        );
    }

    #[test]
    fn test_model_path_beats_preset() {
        // TOML has both `model_path` and `preset` → model_path wins.
        let _lock = AUX_ENV_LOCK.lock().unwrap();
        let tmp = tempfile::TempDir::new().unwrap();
        write_stub_splade_bundle(tmp.path());
        std::env::remove_var("CQS_SPLADE_MODEL");
        let _g = EnvGuard {
            key: "CQS_SPLADE_MODEL",
        };

        let model_path = tmp.path().join("model.onnx");
        let resolved = resolve(
            AuxModelKind::Splade,
            None,
            "CQS_SPLADE_MODEL",
            Some("ensembledistil"),
            Some(&model_path),
            None,
            "ensembledistil",
        )
        .unwrap();
        assert_eq!(resolved.preset, None);
        assert_eq!(resolved.model_path, model_path);
        // tokenizer_path inferred as sibling
        assert_eq!(resolved.tokenizer_path, tmp.path().join("tokenizer.json"));
    }

    #[test]
    fn test_reranker_preset_sets_repo() {
        // Reranker preset resolution must populate `repo` so the HF fetcher
        // knows to go to the Hub.
        let _lock = AUX_ENV_LOCK.lock().unwrap();
        std::env::remove_var("CQS_RERANKER_MODEL");
        let _g = EnvGuard {
            key: "CQS_RERANKER_MODEL",
        };

        let resolved = resolve(
            AuxModelKind::Reranker,
            None,
            "CQS_RERANKER_MODEL",
            None,
            None,
            None,
            "ms-marco-minilm",
        )
        .unwrap();
        assert_eq!(resolved.preset.as_deref(), Some("ms-marco-minilm"));
        assert_eq!(
            resolved.repo.as_deref(),
            Some("cross-encoder/ms-marco-MiniLM-L-6-v2")
        );
    }

    #[test]
    fn test_reranker_env_repo_id_accepted() {
        // For reranker, a non-path env value is treated as an HF repo id.
        let _lock = AUX_ENV_LOCK.lock().unwrap();
        std::env::set_var("CQS_RERANKER_MODEL", "some-org/some-model");
        let _g = EnvGuard {
            key: "CQS_RERANKER_MODEL",
        };

        let resolved = resolve(
            AuxModelKind::Reranker,
            None,
            "CQS_RERANKER_MODEL",
            None,
            None,
            None,
            "ms-marco-minilm",
        )
        .unwrap();
        assert_eq!(resolved.repo.as_deref(), Some("some-org/some-model"));
    }

    #[test]
    fn test_splade_env_repo_id_rejected() {
        // SPLADE has no HF-fetch path — a bare repo id in env must error.
        let _lock = AUX_ENV_LOCK.lock().unwrap();
        std::env::set_var("CQS_SPLADE_MODEL", "naver/splade-cocondenser");
        let _g = EnvGuard {
            key: "CQS_SPLADE_MODEL",
        };

        let err = resolve(
            AuxModelKind::Splade,
            None,
            "CQS_SPLADE_MODEL",
            None,
            None,
            None,
            "ensembledistil",
        )
        .unwrap_err();
        assert!(
            matches!(err, AuxModelError::NotFound(_)),
            "SPLADE must reject non-path overrides"
        );
    }

    #[test]
    fn test_tokenizer_path_without_model_path_errors() {
        let _lock = AUX_ENV_LOCK.lock().unwrap();
        std::env::remove_var("CQS_SPLADE_MODEL");
        let _g = EnvGuard {
            key: "CQS_SPLADE_MODEL",
        };
        let tok = Path::new("/some/tokenizer.json");
        let err = resolve(
            AuxModelKind::Splade,
            None,
            "CQS_SPLADE_MODEL",
            None,
            None,
            Some(tok),
            "ensembledistil",
        )
        .unwrap_err();
        assert!(matches!(err, AuxModelError::InvalidConfig(_)));
    }

    #[test]
    fn test_unknown_preset_errors() {
        let _lock = AUX_ENV_LOCK.lock().unwrap();
        std::env::remove_var("CQS_SPLADE_MODEL");
        let _g = EnvGuard {
            key: "CQS_SPLADE_MODEL",
        };
        let err = resolve(
            AuxModelKind::Splade,
            None,
            "CQS_SPLADE_MODEL",
            Some("does-not-exist"),
            None,
            None,
            "ensembledistil",
        )
        .unwrap_err();
        assert!(
            matches!(err, AuxModelError::UnknownPreset { .. }),
            "unknown preset must surface as UnknownPreset"
        );
    }

    #[test]
    fn test_empty_env_falls_through() {
        // Empty env var is equivalent to unset (matches splade behavior).
        let _lock = AUX_ENV_LOCK.lock().unwrap();
        std::env::set_var("CQS_SPLADE_MODEL", "");
        let _g = EnvGuard {
            key: "CQS_SPLADE_MODEL",
        };
        let resolved = resolve(
            AuxModelKind::Splade,
            None,
            "CQS_SPLADE_MODEL",
            None,
            None,
            None,
            "ensembledistil",
        )
        .unwrap();
        assert_eq!(resolved.preset.as_deref(), Some("ensembledistil"));
    }

    #[test]
    fn test_preset_registry_direct_splade() {
        let cfg = preset(AuxModelKind::Splade, "ensembledistil").unwrap();
        assert_eq!(cfg.preset.as_deref(), Some("ensembledistil"));
        let cfg = preset(AuxModelKind::Splade, "splade-code-0.6b").unwrap();
        assert_eq!(cfg.preset.as_deref(), Some("splade-code-0.6b"));
        assert!(preset(AuxModelKind::Splade, "nope").is_none());
    }

    #[test]
    fn test_preset_registry_direct_reranker() {
        let cfg = preset(AuxModelKind::Reranker, "ms-marco-minilm").unwrap();
        assert_eq!(cfg.preset.as_deref(), Some("ms-marco-minilm"));
        assert_eq!(
            cfg.repo.as_deref(),
            Some("cross-encoder/ms-marco-MiniLM-L-6-v2")
        );
        assert!(preset(AuxModelKind::Reranker, "nope").is_none());
    }

    #[test]
    fn test_default_preset_name() {
        assert_eq!(default_preset_name(AuxModelKind::Splade), "ensembledistil");
        assert_eq!(
            default_preset_name(AuxModelKind::Reranker),
            "ms-marco-minilm"
        );
    }

    #[test]
    fn test_is_path_like() {
        assert!(is_path_like("/abs"));
        assert!(is_path_like("~/home"));
        assert!(!is_path_like("org/model"));
        assert!(!is_path_like("ms-marco-minilm"));
    }

    /// BUG-D.7: Windows users couldn't pass `--reranker-model C:\Models\splade`
    /// — the string was treated as an HF repo id and shipped to Hub fetcher.
    /// `Path::is_absolute()` recognizes drive-letter paths on Windows builds.
    #[test]
    #[cfg(windows)]
    fn is_path_like_accepts_windows_drive_letter() {
        assert!(is_path_like("C:\\Models\\splade"));
        assert!(is_path_like("D:/foo/bar"));
    }

    /// BUG-D.7: UNC paths (`\\server\share\splade`) must also route through
    /// the local-path branch, not the HF Hub fetch path. The leading `\\\\`
    /// check works on every platform — `Path::is_absolute()` agrees on
    /// Windows; on Unix the explicit prefix check covers it.
    #[test]
    fn is_path_like_accepts_unc_paths() {
        assert!(is_path_like("\\\\server\\share\\splade"));
    }

    /// Regression: the existing unix-absolute and tilde behavior must not
    /// regress when the new Windows branches were added.
    #[test]
    fn is_path_like_still_accepts_unix_absolute_and_tilde() {
        assert!(is_path_like("/usr/local/models/splade"));
        assert!(is_path_like("~/models/splade"));
    }

    /// Negative: bare HF repo ids like `org/model` must still be rejected
    /// by the path-like discriminator so the reranker resolver routes them
    /// to the Hub fetcher.
    #[test]
    fn is_path_like_rejects_repo_id() {
        assert!(!is_path_like("mixedbread-ai/mxbai-edge-colbert-v0-32m"));
        assert!(!is_path_like("naver/splade-cocondenser-ensembledistil"));
    }

    #[test]
    fn test_expand_tilde() {
        let home = dirs::home_dir().unwrap();
        assert_eq!(expand_tilde("~/foo"), home.join("foo"));
        assert_eq!(expand_tilde("/abs"), PathBuf::from("/abs"));
        assert_eq!(expand_tilde("relative"), PathBuf::from("relative"));
    }

    #[test]
    fn test_explicit_model_path_missing_errors() {
        let _lock = AUX_ENV_LOCK.lock().unwrap();
        std::env::remove_var("CQS_SPLADE_MODEL");
        let _g = EnvGuard {
            key: "CQS_SPLADE_MODEL",
        };
        let missing = Path::new("/definitely/does/not/exist/model.onnx");
        let err = resolve(
            AuxModelKind::Splade,
            None,
            "CQS_SPLADE_MODEL",
            None,
            Some(missing),
            None,
            "ensembledistil",
        )
        .unwrap_err();
        assert!(matches!(err, AuxModelError::NotFound(_)));
    }
}
