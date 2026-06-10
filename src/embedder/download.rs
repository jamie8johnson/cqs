//! Model + tokenizer download / fingerprint helpers.
//!
//! Split out of the former monolithic `embedder/mod.rs` (issue #1691).

use super::*;
use std::path::{Path, PathBuf};

/// Download model and tokenizer from HuggingFace Hub (or load from `CQS_ONNX_DIR`).
pub(crate) fn ensure_model(config: &ModelConfig) -> Result<(PathBuf, PathBuf), EmbedderError> {
    // CQS_ONNX_DIR: bypass HF download, load from local directory.
    // Directory must contain model.onnx and tokenizer.json.
    if let Ok(dir) = std::env::var("CQS_ONNX_DIR") {
        let dir = dunce::canonicalize(PathBuf::from(&dir)).unwrap_or_else(|_| PathBuf::from(dir));
        let model_path = dir.join(&config.onnx_path);
        let tokenizer_path = dir.join(&config.tokenizer_path);
        // Verify joined paths stay inside CQS_ONNX_DIR (symlink/traversal defense)
        for (label, path) in [("model", &model_path), ("tokenizer", &tokenizer_path)] {
            if let Ok(canonical) = dunce::canonicalize(path) {
                if !canonical.starts_with(&dir) {
                    return Err(EmbedderError::ModelNotFound(format!(
                        "SEC-3: {} path escapes CQS_ONNX_DIR: {} resolves to {}",
                        label,
                        path.display(),
                        canonical.display()
                    )));
                }
            }
        }
        if model_path.exists() && tokenizer_path.exists() {
            tracing::info!(dir = %dir.display(), "Using local ONNX model directory");
            return Ok((model_path, tokenizer_path));
        }
        // Try flat layout (model.onnx + tokenizer.json in same dir)
        let flat_model = dir.join("model.onnx");
        let flat_tok = dir.join("tokenizer.json");
        if flat_model.exists() && flat_tok.exists() {
            tracing::info!(dir = %dir.display(), "Using local ONNX model directory (flat)");
            return Ok((flat_model, flat_tok));
        }
        tracing::warn!(dir = %dir.display(), "CQS_ONNX_DIR set but model files not found, falling back to HF download");
    }

    use hf_hub::api::sync::ApiBuilder;

    // hf-hub defaults to max_retries=0 — a single transient ureq error
    // (TLS handshake glitch, HTTP/2 reset, throttling) aborts the download
    // with no retry. The Python `huggingface-cli` retries internally and
    // succeeds, so the same file from the same server can fail in cqs and
    // succeed in Python. 5 retries make >2 GB external-data downloads
    // (Gemma, Qwen3) reliable: a silently-failed `model.onnx_data` fetch
    // otherwise makes ORT panic at session init with "cannot get file size".
    let api = ApiBuilder::from_env()
        .with_retries(5)
        .build()
        .map_err(|e| EmbedderError::ModelDownload(e.to_string()))?;
    let repo = api.model(config.repo.clone());

    let model_path = repo
        .get(&config.onnx_path)
        .map_err(|e| EmbedderError::ModelDownload(e.to_string()))?;
    let tokenizer_path = repo
        .get(&config.tokenizer_path)
        .map_err(|e| EmbedderError::ModelDownload(e.to_string()))?;

    // Fetch the ONNX external-data sidecar for models that exceed the 2GB
    // protobuf limit. The Rust ONNX Runtime expects the .onnx_data file to
    // sit next to model.onnx; without it, session init fails with
    // "filesystem error: cannot get file size" when the graph references
    // external tensors. Most presets (BGE, E5, etc.) ship a single
    // self-contained model.onnx and do not have this file — the sidecar
    // attempt returns a 404 wrapped in `ApiError::RequestError`. That's
    // expected for self-contained models and gets silenced at debug level.
    //
    // Anything other than a clean 404 (network error, IoError,
    // LockAcquisition, etc.) is unexpected: either the operator is on
    // an external-data preset and the file genuinely couldn't be fetched
    // (broken setup), or there's a transient that even retries didn't
    // recover from. Either way the operator needs to see it — log at
    // warn so it surfaces at the default RUST_LOG level. The pipeline
    // continues; if ORT later panics with "cannot get file size", the
    // warn line is the breadcrumb.
    let external_data_path = format!("{}_data", config.onnx_path);
    match repo.get(&external_data_path) {
        Ok(_) => {} // Sidecar fetched (or already cached) — common path for Gemma / Qwen3.
        Err(e) if is_likely_not_found(&e) => {
            tracing::debug!(
                file = %external_data_path,
                error = %e,
                "ONNX external-data sidecar not present in repo (expected for self-contained models)"
            );
        }
        Err(e) => {
            tracing::warn!(
                file = %external_data_path,
                error = %e,
                "ONNX external-data sidecar fetch failed unexpectedly. \
                 If the model exceeds 2GB, ORT will panic with 'cannot get file size' \
                 at session init. Check network, HF auth, or run \
                 `hf download <repo> --include='{external_data_path}'` manually."
            );
        }
    }

    // Verify checksums (skip if already verified via marker file)
    if !MODEL_BLAKE3.is_empty() || !TOKENIZER_BLAKE3.is_empty() {
        let marker = model_path
            .parent()
            .unwrap_or(Path::new("."))
            .join(".cqs_verified");
        let expected_marker = format!("{}\n{}", MODEL_BLAKE3, TOKENIZER_BLAKE3);
        let already_verified = std::fs::read_to_string(&marker)
            .map(|s| s == expected_marker)
            .unwrap_or(false);

        if !already_verified {
            if !MODEL_BLAKE3.is_empty() {
                verify_checksum(&model_path, MODEL_BLAKE3)?;
            }
            if !TOKENIZER_BLAKE3.is_empty() {
                verify_checksum(&tokenizer_path, TOKENIZER_BLAKE3)?;
            }
            // Write marker after successful verification. Surface failure at
            // warn so operators see why subsequent cold starts re-blake3 the
            // model file (~600 MB at 4s+ per startup).
            if let Err(e) = std::fs::write(&marker, &expected_marker) {
                tracing::warn!(
                    path = %marker.display(),
                    error = %e,
                    "Failed to write checksum-verified marker — model will be re-verified on next session"
                );
            }
        }
    }

    Ok((model_path, tokenizer_path))
}

/// Heuristic — was an `hf_hub::api::sync::ApiError` likely a 404 / "file not
/// in repo" rather than a real failure?
///
/// `hf_hub` wraps the HTTP layer behind ureq and exposes most errors as
/// `RequestError(Box<ureq::Error>)`. Distinguishing 404 cleanly would
/// require pattern-matching the inner ureq error, which is version-specific
/// and exposes the dependency surface. Instead we string-match the
/// rendered error: `ureq::Error::Status(404, _)` formats with "404" or
/// "status code 404". Good-enough for the warn-vs-debug split — if a 404
/// ever stops formatting that way, we just get a warn for a self-contained
/// preset, which is annoying but not wrong. False-positive direction
/// (treating a real failure as 404) is the more dangerous one but
/// requires the error message to literally contain "404", which a network
/// failure normally won't.
pub(crate) fn is_likely_not_found(err: &hf_hub::api::sync::ApiError) -> bool {
    let s = err.to_string();
    s.contains("404") || s.contains("Not Found") || s.contains("not found")
}

/// Verify file checksum using blake3
pub(crate) fn verify_checksum(path: &Path, expected: &str) -> Result<(), EmbedderError> {
    let mut file =
        std::fs::File::open(path).map_err(|e| EmbedderError::ModelNotFound(e.to_string()))?;
    let mut hasher = blake3::Hasher::new();
    std::io::copy(&mut file, &mut hasher)
        .map_err(|e| EmbedderError::ModelNotFound(e.to_string()))?;
    let actual = hasher.finalize().to_hex().to_string();

    if actual != expected {
        return Err(EmbedderError::ChecksumMismatch {
            path: path.display().to_string(),
            expected: expected.to_string(),
            actual,
        });
    }
    Ok(())
}
