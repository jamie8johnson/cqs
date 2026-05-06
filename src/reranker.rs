//! Cross-encoder re-ranking for second-pass scoring
//!
//! Reorders search results using a cross-encoder model that scores
//! (query, passage) pairs directly, producing more accurate rankings
//! than embedding cosine similarity alone.
//!
//! Uses `cross-encoder/ms-marco-MiniLM-L-6-v2` (~91MB ONNX, 22M params).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use once_cell::sync::OnceCell;
use ort::session::Session;

use crate::aux_model::{self, AuxModelKind};
use crate::config::AuxModelSection;
use crate::embedder::{
    create_session, pad_2d_i64_from_encodings, select_provider, ExecutionProvider,
};
use crate::store::SearchResult;

/// Filename within the HF repo layout — kept local so
/// [`Reranker::model_paths`] can still construct the expected layout when
/// the HF Hub fetch succeeds. Matches the convention in
/// [`crate::aux_model::config_from_dir`] for `AuxModelKind::Reranker`.
const MODEL_FILE: &str = "onnx/model.onnx";
const TOKENIZER_FILE: &str = "tokenizer.json";
/// SHL-V1.36-6: HuggingFace `config.json` contains `hidden_size`, which the
/// batch-size formula needs to scale per-tensor activation memory across
/// rerankers of different model widths. Resolved alongside `MODEL_FILE` /
/// `TOKENIZER_FILE` in [`OnnxReranker::model_paths`].
const CONFIG_FILE: &str = "config.json";

/// Reference `hidden_size`, paired with [`DEFAULT_RERANKER_BATCH`] and
/// [`REFERENCE_MAX_LENGTH`]. ms-marco-MiniLM-L-6-v2's hidden_size is 384;
/// the batch-size formula scales `(REFERENCE_HIDDEN / hidden).max(0.25)`
/// alongside the seq factor. Custom rerankers with hidden=768 (e.g.
/// MiniLM-L-12) get `0.5×` the seq-implied batch — keeps per-tensor memory
/// constant.
const REFERENCE_HIDDEN_SIZE: usize = 384;

// blake3 checksums -- empty to skip validation (set after pinning a model version)
const MODEL_BLAKE3: &str = "";
const TOKENIZER_BLAKE3: &str = "";

/// Default batch size for reranker ORT runs.
///
/// Caps the candidate set fed to each `session.run()` call so a large `k`
/// (e.g. `--rerank-k 100` with `max_length=512`) doesn't allocate a single
/// `[100, 512]` token tensor that OOMs on small GPUs or after SPLADE has
/// claimed VRAM. Mirrors the `CQS_EMBED_BATCH_SIZE=64` pattern in the
/// embed path; 32 is conservative because cross-encoder runs produce larger
/// activations than plain encoder forward passes.
const DEFAULT_RERANKER_BATCH: usize = 32;

/// Reranker reference seq-length, paired with [`DEFAULT_RERANKER_BATCH`].
/// The [`reranker_batch_size`] formula scales the baseline batch by
/// `(REFERENCE_MAX_LENGTH / max_length).max(0.25)` so a custom reranker
/// running at 2048 tokens halves the batch (32 → 16) instead of OOMing
/// at the documented default.
const REFERENCE_MAX_LENGTH: usize = 512;

/// Maximum number of candidates per ORT `session.run()` in the reranker.
///
/// SHL-V1.36-6 (#1463 follow-up): scales the baseline 32 by the loaded
/// model's `(hidden_size, max_length)` so per-tensor activation memory
/// stays roughly constant across rerankers. Long-context rerankers
/// (`CQS_RERANKER_MAX_LENGTH=2048`) and wider rerankers
/// (hidden_size=768 vs the 384 baseline) used to OOM 8 GB GPUs at the
/// documented default 32. Now both factors collapse onto the same
/// formula:
///
/// ```text
/// batch = 32 * (REFERENCE_HIDDEN / hidden).max(0.25)
///            * (REFERENCE_MAX_LENGTH / max_length).max(0.25)
/// ```
///
/// rounded to a power of two clamped to `[1, 256]`. Each factor floors
/// at 0.25 so neither dim alone collapses the batch to 1 on extreme
/// models — net floor is 32 * 0.25 * 0.25 = 2.
///
/// `hidden_size` is probed from `config.json` at session-init time
/// (see [`OnnxReranker::resolve_hidden_size`]); on a fetch failure or
/// missing field the call falls back to [`REFERENCE_HIDDEN_SIZE`],
/// which preserves the historic batch=32 for ms-marco-MiniLM-L-6-v2.
///
/// `CQS_RERANKER_BATCH` env wins regardless of either dim — operators
/// with workloads they understand can pin a value.
fn reranker_batch_size(max_length: usize, hidden_size: usize) -> usize {
    // P2.4: route through shared `parse_env_usize` so behavior matches the
    // 24 other CQS_* knobs (missing/empty/garbage/zero -> default).
    if std::env::var_os("CQS_RERANKER_BATCH").is_some() {
        return crate::limits::parse_env_usize("CQS_RERANKER_BATCH", DEFAULT_RERANKER_BATCH);
    }
    let baseline = DEFAULT_RERANKER_BATCH as f64;
    let max_length = max_length.max(1) as f64;
    let hidden_size = hidden_size.max(1) as f64;
    let seq_factor = (REFERENCE_MAX_LENGTH as f64 / max_length).max(0.25);
    let hidden_factor = (REFERENCE_HIDDEN_SIZE as f64 / hidden_size).max(0.25);
    let scaled = (baseline * seq_factor * hidden_factor).max(1.0) as usize;
    let rounded = scaled.next_power_of_two().clamp(1, 256);
    if rounded != DEFAULT_RERANKER_BATCH {
        tracing::debug!(
            max_length = max_length as usize,
            hidden_size = hidden_size as usize,
            scaled,
            rounded,
            "reranker_batch_size: scaling baseline by (hidden_size, max_length)"
        );
    }
    rounded
}

/// Resolve the reranker model source via the shared auxiliary-model
/// resolver, threading an optional `[reranker]` config section through the
/// same preset registry SPLADE uses. Returns a fully-populated
/// [`aux_model::AuxModelConfig`] — the caller (`model_paths`) dispatches
/// on `repo.is_some()` to decide between local-dir and HF Hub fetch paths.
///
/// Precedence: CLI → `CQS_RERANKER_MODEL` → `[reranker] model_path` →
/// `[reranker] preset` → hardcoded `ms-marco-minilm`.
fn resolve_reranker(
    section: Option<&AuxModelSection>,
) -> Result<aux_model::AuxModelConfig, RerankerError> {
    let preset = section.and_then(|s| s.preset.as_deref());
    let model_path = section.and_then(|s| s.model_path.as_deref());
    let tokenizer_path = section.and_then(|s| s.tokenizer_path.as_deref());
    aux_model::resolve(
        AuxModelKind::Reranker,
        None,
        "CQS_RERANKER_MODEL",
        preset,
        model_path,
        tokenizer_path,
        aux_model::default_preset_name(AuxModelKind::Reranker),
    )
    .map_err(|e| RerankerError::ModelDownload(e.to_string()))
}

#[derive(Debug, thiserror::Error)]
pub enum RerankerError {
    #[error("Model download failed: {0}")]
    ModelDownload(String),
    #[error("Tokenizer error: {0}")]
    Tokenizer(String),
    #[error("Inference error: {0}")]
    Inference(String),
    #[error("Checksum mismatch for {path}: expected {expected}, got {actual}")]
    ChecksumMismatch {
        path: String,
        expected: String,
        actual: String,
    },
    /// Caller supplied mismatched-length / otherwise invalid arguments.
    /// Distinct from `Inference` so callers can pattern-match the bug.
    #[error("Invalid arguments: {0}")]
    InvalidArguments(String),
}

/// CQ-V1.30.1-5 (P3-CQ-2): route a stringified ORT message into
/// [`Inference`](RerankerError::Inference) so the shared
/// [`crate::ort_helpers::ort_err`] helper can hand back the right
/// variant for reranker call sites. Sealed trait, not `From<String>`,
/// so `.map_err(ort_err)` type inference isn't ambiguous against the
/// reflexive `From<T> for T` impl.
impl crate::ort_helpers::FromOrtMessage for RerankerError {
    fn from_ort_message(msg: String) -> Self {
        Self::Inference(msg)
    }
}

use crate::ort_helpers::ort_err;

/// EX-V1.30.1-8 (#1220): pluggable second-pass scoring trait.
///
/// Holders should keep rerankers as `Arc<dyn Reranker>` so any of the
/// shipped impls (`OnnxReranker`, `NoopReranker`, `LlmReranker`) can
/// drop in without touching the production search path. Adding a
/// fourth impl (BM25 baseline, dot-product over a different embedder,
/// API-served scoring service) is one new struct + one `impl Reranker`
/// block + one wire-up at the construction site.
///
/// Concurrency: the production hot path uses `Arc<dyn Reranker>`, so
/// the trait requires `Send + Sync`. `OnnxReranker` is internally
/// thread-safe via `Mutex` around the ONNX session; the trait surface
/// borrows `&self` so callers don't need to lock the holder.
pub trait Reranker: Send + Sync {
    /// Re-rank `results` in place, scoring each via this reranker's
    /// `(query, content)` model. Default contract: writes scores onto
    /// each result, sorts descending, truncates to `limit`.
    /// `results.len() <= 1` is a no-op.
    fn rerank(
        &self,
        query: &str,
        results: &mut Vec<SearchResult>,
        limit: usize,
    ) -> Result<(), RerankerError>;

    /// Re-rank scoring `(query, passages[i])` instead of
    /// `(query, results[i].chunk.content)`. `passages.len() ==
    /// results.len()` is a hard requirement; impls return
    /// `RerankerError::InvalidArguments` on mismatch.
    fn rerank_with_passages(
        &self,
        query: &str,
        results: &mut Vec<SearchResult>,
        passages: &[&str],
        limit: usize,
    ) -> Result<(), RerankerError>;

    /// Drop any cached state (model session, tokenizer, network handles)
    /// so memory returns to the OS. Called from idle-eviction loops in
    /// the watch/serve daemons. Default: no-op (trait impls without
    /// resident state get this for free).
    fn clear_session(&self) {}
}

/// Cross-encoder reranker for second-pass scoring (the ONNX implementation).
///
/// Lazy-loads the model on first use, same pattern as [`crate::Embedder`].
/// Scores (query, passage) pairs with a cross-encoder, then re-sorts results.
///
/// EX-V1.30.1-8 (#1220): renamed from `Reranker` to `OnnxReranker` when the
/// trait was extracted. The trait is the new `Reranker`; concrete callers
/// that need ONNX specifically construct via `OnnxReranker::with_section`.
pub struct OnnxReranker {
    session: Mutex<Option<Session>>,
    /// Lazy-loaded tokenizer.
    ///
    /// RM-V1.25-15: `Mutex<Option<Arc<Tokenizer>>>` so `clear_session` can
    /// drop the tokenizer (~20MB for ms-marco MiniLM) alongside the ONNX
    /// session. Callers receive an `Arc<Tokenizer>` clone and release the
    /// mutex before running inference.
    tokenizer: Mutex<Option<Arc<tokenizers::Tokenizer>>>,
    model_paths: OnceCell<(PathBuf, PathBuf)>,
    provider: ExecutionProvider,
    max_length: usize,
    /// Whether the loaded ONNX session expects a `token_type_ids` input.
    /// BERT-family models do; RoBERTa-family (UniXcoder, CodeBERT, all
    /// XLM-R variants) do not. Computed at session-init time by inspecting
    /// the model's input names. `None` means "session not yet loaded."
    expects_token_type_ids: Mutex<Option<bool>>,
    /// SHL-V1.36-6: model's `hidden_size`, probed from `config.json` at
    /// session-init time. Falls back to [`REFERENCE_HIDDEN_SIZE`] (384)
    /// when the file is missing or malformed — preserves the historic
    /// batch=32 for ms-marco-MiniLM-L-6-v2 if the probe fails.
    /// `OnceCell` so the probe runs at most once per reranker instance.
    hidden_size: OnceCell<usize>,
    /// Cached config-file `[reranker]` section so `resolve_reranker` honours
    /// `preset` / `model_path` / `tokenizer_path` set in `.cqs.toml` (P1.7).
    section: Option<AuxModelSection>,
}

impl OnnxReranker {
    /// Create a new reranker with lazy model loading (config-less; CLI/env only).
    pub fn new() -> Result<Self, RerankerError> {
        Self::with_section(None)
    }

    /// Create a reranker, threading a `[reranker]` config section through to
    /// `resolve_reranker` so `.cqs.toml` preset / model_path are honoured (P1.7).
    pub fn with_section(section: Option<AuxModelSection>) -> Result<Self, RerankerError> {
        let provider = select_provider();
        let max_length = match std::env::var("CQS_RERANKER_MAX_LENGTH") {
            Ok(val) => match val.parse::<usize>() {
                Ok(len) => {
                    tracing::info!(max_length = len, "Using custom reranker max_length");
                    len
                }
                Err(e) => {
                    tracing::warn!(
                        value = %val,
                        error = %e,
                        "Invalid CQS_RERANKER_MAX_LENGTH, using default 512"
                    );
                    512
                }
            },
            Err(_) => 512,
        };
        Ok(Self {
            session: Mutex::new(None),
            tokenizer: Mutex::new(None),
            model_paths: OnceCell::new(),
            provider,
            max_length,
            expects_token_type_ids: Mutex::new(None),
            hidden_size: OnceCell::new(),
            section,
        })
    }

    /// Compute cross-encoder scores for (query, passage) pairs.
    ///
    /// Returns `Some(scores)` on success, or `None` when tokenization produced
    /// zero-length encodings across all passages (nothing to score).
    /// `scores.len() == passages.len()` on `Some(...)`.
    ///
    /// PF-V1.25-5: extracted so `rerank` can feed passages borrowed directly
    /// from `&Vec<SearchResult>` without cloning contents, then apply scores
    /// via `apply_rerank_scores` in a subsequent `&mut` scope.
    ///
    /// Issue #963: passages are chunked into `CQS_RERANKER_BATCH`-sized
    /// groups (default 32) before feeding each chunk to `session.run()`. This
    /// keeps the `[chunk_len, max_length]` token tensor bounded so large `k`
    /// values don't OOM on small GPUs or after SPLADE has claimed VRAM.
    /// Scoring semantics are preserved — each candidate gets the same
    /// cross-encoder score, just computed in smaller ORT runs.
    fn compute_scores_opt(
        &self,
        query: &str,
        passages: &[&str],
    ) -> Result<Option<Vec<f32>>, RerankerError> {
        let tokenizer = self.tokenizer()?;

        // 1. Tokenize (query, passage) pairs once up front. Tokenization is
        //    cheap relative to ORT inference and doing it here lets us
        //    short-circuit (return None) when the entire input is degenerate,
        //    matching the pre-#963 semantics.
        let encodings: Vec<tokenizers::Encoding> = passages
            .iter()
            .map(|passage| {
                tokenizer
                    .encode((query, *passage), true)
                    .map_err(|e| RerankerError::Tokenizer(e.to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?;

        let overall_max = encodings
            .iter()
            .map(|e| e.get_ids().len())
            .max()
            .unwrap_or(0)
            .min(self.max_length);
        if overall_max == 0 {
            return Ok(None); // Nothing to score — empty tokenization
        }

        let batch_cap = reranker_batch_size(self.max_length, self.resolve_hidden_size());
        let mut scores = Vec::with_capacity(passages.len());
        for chunk in encodings.chunks(batch_cap) {
            scores.extend(self.run_chunk(chunk)?);
        }
        Ok(Some(scores))
    }

    /// Run one reranker batch: build tensors from `chunk` and score via ORT.
    ///
    /// `chunk` is a slice of tokenized (query, passage) encodings sized to at
    /// most `CQS_RERANKER_BATCH`. The per-chunk `max_len` is the longest
    /// encoding in this chunk capped at `self.max_length`, so shorter chunks
    /// use smaller tensors.
    ///
    /// Returns one score per encoding in `chunk`.
    fn run_chunk(&self, chunk: &[tokenizers::Encoding]) -> Result<Vec<f32>, RerankerError> {
        // P3-1 (audit v1.33.0): track per-chunk wall-clock so operators
        // tuning `CQS_RERANKER_BATCH` or chasing tail latency can see
        // per-chunk shape. Owns ~98% of reranker latency (ORT session.run).
        let start = std::time::Instant::now();
        let batch_size = chunk.len();
        debug_assert!(batch_size > 0, "run_chunk called with empty chunk");

        // Build per-chunk padded tensors. PERF-V1.33-3 / #1377: pull
        // `max_len` from encodings directly and feed the encodings to
        // `pad_2d_i64_from_encodings` below — no intermediate
        // `Vec<Vec<i64>>` per field.
        let max_len = chunk
            .iter()
            .map(|e| e.get_ids().len())
            .max()
            .unwrap_or(0)
            .min(self.max_length);
        if max_len == 0 {
            // AC-V1.36-3 / P2-9 / P2-11: this chunk's passages all tokenized
            // empty but the aggregate check in compute_scores_opt already
            // guaranteed overall_max > 0. Return sigmoid(0)=0.5 for these rows
            // so the surviving non-empty cohort still carries ranking signal,
            // BUT warn so operators know the resulting rank order may
            // interleave 0.5 synthetic scores into the middle of the
            // cross-encoder distribution. Proper fix (return Option<f32>
            // through to apply_rerank_scores so we can fall back to the
            // input cosine for empty rows) is follow-up.
            tracing::warn!(
                batch_size,
                "Reranker chunk all-empty after tokenization — emitting sigmoid(0)=0.5 fallback; \
                 ranking may interleave synthetic scores with cross-encoder scores"
            );
            return Ok(vec![sigmoid(0.0); batch_size]);
        }

        // token_type_ids come from the tokenizer — BERT-family rerankers use
        // them to distinguish query (0) from passage (1). Zeroing them out (the
        // prior behavior) silently broke fine-tuned models that learned to
        // use the segment signal (caught during reranker v2 eval: gold chunks
        // got pushed below negatives because the model saw "query query" when
        // the tokenizer had emitted "query passage"). RoBERTa-family models
        // (UniXcoder, CodeBERT, XLM-R) don't accept this input at all —
        // session() detects which family the loaded model is and we skip
        // building the type tensor when the session doesn't expect it.
        let mut session_guard = self.session()?;
        let session = session_guard
            .as_mut()
            .expect("session() guarantees initialized after Ok return");
        let expects_tti = self
            .expects_token_type_ids
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .unwrap_or(true); // session() always sets this; fallback to true matches BERT default

        // P3-1: per-chunk span carries shape data for journal correlation.
        let _chunk_span =
            tracing::debug_span!("reranker_run_chunk", batch_size, max_len, expects_tti).entered();

        let ids_arr = pad_2d_i64_from_encodings(chunk, |e| e.get_ids(), max_len, 0);
        let mask_arr = pad_2d_i64_from_encodings(chunk, |e| e.get_attention_mask(), max_len, 0);

        use ort::value::Tensor;
        let ids_tensor = Tensor::from_array(ids_arr).map_err(ort_err)?;
        let mask_tensor = Tensor::from_array(mask_arr).map_err(ort_err)?;

        let outputs = if expects_tti {
            let type_arr = pad_2d_i64_from_encodings(chunk, |e| e.get_type_ids(), max_len, 0);
            let type_tensor = Tensor::from_array(type_arr).map_err(ort_err)?;
            session
                .run(ort::inputs![
                    "input_ids" => ids_tensor,
                    "attention_mask" => mask_tensor,
                    "token_type_ids" => type_tensor,
                ])
                .map_err(ort_err)?
        } else {
            session
                .run(ort::inputs![
                    "input_ids" => ids_tensor,
                    "attention_mask" => mask_tensor,
                ])
                .map_err(ort_err)?
        };

        // Extract logits, apply sigmoid.
        if outputs.len() == 0 {
            return Err(RerankerError::Inference(
                "ONNX model produced no outputs".to_string(),
            ));
        }
        let (shape, data) = outputs[0].try_extract_tensor::<f32>().map_err(ort_err)?;

        // AC-V1.29-6: ORT's `shape[1]` is `i64` and can be -1 when a
        // dynamic axis is unbound (or, in principle, any negative value
        // the model exporter emits). Casting `-1 as usize` gives
        // `usize::MAX` — the subsequent `batch_size * stride` then wraps,
        // `data.len() < expected_len` flips direction, and we read past
        // the buffer. Guard the cast first, then use `checked_mul` so a
        // large legitimate stride can't silently overflow either.
        let stride = if shape.len() == 2 {
            let dim = shape[1];
            if dim < 0 {
                return Err(RerankerError::Inference(format!(
                    "Model returned negative output dim {dim} (dynamic axis not bound?)"
                )));
            }
            dim as usize
        } else {
            1
        };
        if stride == 0 {
            return Err(RerankerError::Inference(
                "Model returned zero-width output tensor".to_string(),
            ));
        }
        let expected_len = batch_size.checked_mul(stride).ok_or_else(|| {
            RerankerError::Inference(format!(
                "Reranker output too large: batch_size={batch_size} * stride={stride} overflows usize"
            ))
        })?;
        if data.len() < expected_len {
            return Err(RerankerError::Inference(format!(
                "Model output too short: expected {} elements, got {}",
                expected_len,
                data.len()
            )));
        }

        let scores: Vec<f32> = (0..batch_size).map(|i| sigmoid(data[i * stride])).collect();
        // P3-1: completion event with elapsed_ms so per-chunk latency
        // is queryable without parsing the surrounding span.
        tracing::debug!(
            elapsed_ms = start.elapsed().as_millis() as u64,
            batch_size,
            "run_chunk complete"
        );
        Ok(scores)
    }

    /// Like [`compute_scores_opt`] but returns an empty vec instead of `None`
    /// when tokenization produces zero-length encodings. Used by [`rerank`]
    /// where a degenerate empty input just means a no-op.
    fn compute_scores(&self, query: &str, passages: &[&str]) -> Result<Vec<f32>, RerankerError> {
        if passages.len() <= 1 {
            return Ok(Vec::new());
        }
        Ok(self
            .compute_scores_opt(query, passages)?
            .unwrap_or_default())
    }

    /// Resolve paths to `model.onnx` and `tokenizer.json`.
    ///
    /// Delegates to [`resolve_reranker`] for precedence handling (CLI → env
    /// → TOML `[reranker] model_path` → TOML `[reranker] preset` → hardcoded
    /// default). When the resolver returns a local-path config, the files
    /// are used directly; when it returns an HF repo id, the Hub API fetches
    /// the bundle.
    ///
    /// Local-bundle layout (shared with [`crate::aux_model`]):
    /// `{dir}/onnx/model.onnx` + `{dir}/tokenizer.json` — matches the
    /// HuggingFace cross-encoder repo layout so an unpacked HF checkout
    /// works without surgery.
    fn model_paths(&self) -> Result<&(PathBuf, PathBuf), RerankerError> {
        self.model_paths.get_or_try_init(|| {
            let _span = tracing::info_span!("reranker_model_resolve").entered();

            let resolved = resolve_reranker(self.section.as_ref())?;

            // Local-bundle branch: resolver already verified the directory
            // existed when the override was path-like. For preset/default
            // cases, `repo` is set and we go through the Hub API below.
            if resolved.repo.is_none() {
                let model_path = resolved.model_path;
                let tokenizer_path = resolved.tokenizer_path;
                if !model_path.exists() || !tokenizer_path.exists() {
                    return Err(RerankerError::ModelDownload(format!(
                        "local reranker bundle missing {} or {} (model_path = {})",
                        MODEL_FILE,
                        TOKENIZER_FILE,
                        model_path.display()
                    )));
                }
                tracing::info!(
                    path = %model_path.display(),
                    preset = ?resolved.preset,
                    "Using local reranker model (no HF download)"
                );
                return Ok((model_path, tokenizer_path));
            }

            let repo_id = resolved
                .repo
                .as_deref()
                .expect("repo.is_some() checked above");
            use hf_hub::api::sync::Api;
            let api = Api::new().map_err(|e| RerankerError::ModelDownload(e.to_string()))?;
            let repo = api.model(repo_id.to_string());

            let model_path = repo
                .get(MODEL_FILE)
                .map_err(|e| RerankerError::ModelDownload(e.to_string()))?;
            let tokenizer_path = repo
                .get(TOKENIZER_FILE)
                .map_err(|e| RerankerError::ModelDownload(e.to_string()))?;

            // Verify checksums (skip if already verified via marker file)
            if !MODEL_BLAKE3.is_empty() || !TOKENIZER_BLAKE3.is_empty() {
                let marker = model_path
                    .parent()
                    .unwrap_or(std::path::Path::new("."))
                    .join(".cqs_reranker_verified");
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
                    // Write marker after successful verification.
                    //
                    // EH-V1.30.1-6: surface marker write failures via tracing.
                    // Silently dropping `let _ = ...` means a permission flip
                    // (or a full disk) costs every subsequent launch a
                    // re-checksum of large model files. The verification
                    // itself succeeded, so this is a best-effort cache write
                    // — keep it warn, not error.
                    if let Err(e) = std::fs::write(&marker, &expected_marker) {
                        tracing::warn!(
                            error = %e,
                            path = %marker.display(),
                            "Failed to write reranker verification marker — \
                             next launch will re-verify checksums"
                        );
                    }
                }
            }

            tracing::info!(model = %model_path.display(), "Reranker model ready");
            Ok((model_path, tokenizer_path))
        })
    }

    /// SHL-V1.36-6: probe the loaded model's `hidden_size` from
    /// `config.json` so [`reranker_batch_size`] can scale per-tensor
    /// memory across rerankers of different widths. Cached via the
    /// `hidden_size: OnceCell<usize>` field — at most one probe per
    /// reranker instance.
    ///
    /// Resolution mirrors the layout assumptions in [`Self::model_paths`]:
    ///
    ///   * **Local-bundle** branch — `config.json` lives at
    ///     `model_path.parent().parent() / "config.json"`, i.e., the bundle
    ///     root above the `onnx/` subdir.
    ///   * **HF Hub** branch — fetched via the existing
    ///     `repo.get(CONFIG_FILE)` API used for the model + tokenizer.
    ///
    /// Failure modes (file missing, malformed JSON, no `hidden_size`
    /// field, fetch error) all collapse to the historic
    /// [`REFERENCE_HIDDEN_SIZE`] (384) — preserves the existing
    /// batch=32 for ms-marco-MiniLM-L-6-v2 when the probe can't run.
    /// Surfaces every fallback at `tracing::debug!` so an operator
    /// running with `RUST_LOG=cqs=debug` can correlate batch-size
    /// surprises with the probe.
    fn resolve_hidden_size(&self) -> usize {
        *self.hidden_size.get_or_init(|| {
            let _span = tracing::debug_span!("reranker_resolve_hidden_size").entered();
            match self.probe_hidden_size() {
                Ok(h) => {
                    tracing::debug!(
                        hidden_size = h,
                        "Probed reranker hidden_size from config.json"
                    );
                    h
                }
                Err(e) => {
                    tracing::debug!(
                        error = %e,
                        fallback = REFERENCE_HIDDEN_SIZE,
                        "config.json probe failed; falling back to reference hidden_size"
                    );
                    REFERENCE_HIDDEN_SIZE
                }
            }
        })
    }

    /// Helper for [`Self::resolve_hidden_size`] — does the actual probe.
    /// Returns an error string per failure mode so the caller can log it
    /// and fall back. Side-effect-free apart from the `tracing::debug!`
    /// inside the parse path.
    fn probe_hidden_size(&self) -> Result<usize, String> {
        let resolved =
            resolve_reranker(self.section.as_ref()).map_err(|e| format!("resolve: {e}"))?;
        let config_path = if resolved.repo.is_some() {
            // HF Hub: fetch config.json the same way we fetch model.onnx.
            use hf_hub::api::sync::Api;
            let repo_id = resolved
                .repo
                .as_deref()
                .expect("repo.is_some() checked above");
            let api = Api::new().map_err(|e| format!("hf api: {e}"))?;
            api.model(repo_id.to_string())
                .get(CONFIG_FILE)
                .map_err(|e| format!("hf get config.json: {e}"))?
        } else {
            // Local bundle: `model_path` is `{dir}/onnx/model.onnx`,
            // `config.json` is at `{dir}/config.json` per HF convention.
            let model_path = resolved.model_path;
            model_path
                .parent()
                .and_then(|p| p.parent())
                .map(|dir| dir.join(CONFIG_FILE))
                .ok_or_else(|| format!("model_path has no two parents: {}", model_path.display()))?
        };
        if !config_path.exists() {
            return Err(format!("not found: {}", config_path.display()));
        }
        let raw = std::fs::read_to_string(&config_path)
            .map_err(|e| format!("read {}: {e}", config_path.display()))?;
        let json: serde_json::Value = serde_json::from_str(&raw)
            .map_err(|e| format!("parse {}: {e}", config_path.display()))?;
        let hidden = json
            .get("hidden_size")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| format!("no hidden_size in {}", config_path.display()))?;
        let hidden = usize::try_from(hidden)
            .map_err(|_| format!("hidden_size {hidden} doesn't fit in usize"))?;
        if hidden == 0 {
            return Err(format!("hidden_size = 0 in {}", config_path.display()));
        }
        Ok(hidden)
    }

    /// Get or initialize the ONNX session
    fn session(&self) -> Result<std::sync::MutexGuard<'_, Option<Session>>, RerankerError> {
        let mut guard = self.session.lock().unwrap_or_else(|p| p.into_inner());
        if guard.is_none() {
            let _span = tracing::info_span!("reranker_session_init").entered();
            let (model_path, _) = self.model_paths()?;
            let session = create_session(model_path, self.provider)
                .map_err(|e| RerankerError::Inference(e.to_string()))?;
            // Inspect input names so run_chunk knows whether to send
            // token_type_ids. BERT-family expects it; RoBERTa-family
            // (UniXcoder, CodeBERT, XLM-R) doesn't.
            let has_tti = session
                .inputs()
                .iter()
                .any(|i| i.name() == "token_type_ids");
            *self
                .expects_token_type_ids
                .lock()
                .unwrap_or_else(|p| p.into_inner()) = Some(has_tti);
            tracing::info!(
                expects_token_type_ids = has_tti,
                "Reranker session initialized"
            );
            *guard = Some(session);
        }
        Ok(guard)
    }

    /// Get or initialize the tokenizer.
    ///
    /// RM-V1.25-15: Returns `Arc<Tokenizer>` so callers drop the mutex
    /// before running inference and `clear_session` can replace the inner
    /// slot without racing against encode.
    fn tokenizer(&self) -> Result<Arc<tokenizers::Tokenizer>, RerankerError> {
        {
            let guard = self.tokenizer.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(t) = guard.as_ref() {
                return Ok(Arc::clone(t));
            }
        }
        let (_, tokenizer_path) = self.model_paths()?;
        let _span = tracing::info_span!("reranker_tokenizer_init").entered();
        let loaded = Arc::new(
            tokenizers::Tokenizer::from_file(tokenizer_path)
                .map_err(|e| RerankerError::Tokenizer(e.to_string()))?,
        );
        let mut guard = self.tokenizer.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(existing) = guard.as_ref() {
            return Ok(Arc::clone(existing));
        }
        *guard = Some(Arc::clone(&loaded));
        Ok(loaded)
    }
}

impl Reranker for OnnxReranker {
    /// Re-rank search results using cross-encoder scoring.
    ///
    /// Scores each (query, result.content) pair, re-sorts by score descending,
    /// and truncates to `limit`. No-op for 0 or 1 results.
    fn rerank(
        &self,
        query: &str,
        results: &mut Vec<SearchResult>,
        limit: usize,
    ) -> Result<(), RerankerError> {
        // OB-V1.29-1: entry span parity with `rerank_with_passages` so the
        // CLI `rerank` path shows up in `cqs trace`-style captures with the
        // same tag and fields.
        let _span = tracing::info_span!(
            "rerank",
            count = results.len(),
            limit,
            query_len = query.len()
        )
        .entered();
        // PF-V1.25-5: borrow passages from results directly instead of
        // cloning content strings. The previous impl did
        // `results.iter().map(|r| r.chunk.content.clone()).collect()`,
        // allocating a fresh String per candidate (N allocations × content
        // length bytes each) only to feed them to `rerank_with_passages`.
        // Score computation happens in a scoped borrow so the subsequent
        // `&mut results` write back is valid.
        //
        // We inline the compute-score-then-apply pattern rather than
        // reusing `rerank_with_passages`, because passages that borrow
        // from `results` conflict with `&mut results` at the call site.
        let scores = {
            let passages: Vec<&str> = results.iter().map(|r| r.chunk.content.as_str()).collect();
            self.compute_scores(query, &passages)?
        };
        apply_rerank_scores(results, scores, limit);
        Ok(())
    }

    /// Re-rank search results using custom passage text per result.
    ///
    /// Like [`rerank`](Reranker::rerank) but scores `(query, passages[i])` instead of
    /// `(query, result.content)`. Useful for reranking on NL descriptions or
    /// other derived text. `passages` must have the same length as `results`.
    fn rerank_with_passages(
        &self,
        query: &str,
        results: &mut Vec<SearchResult>,
        passages: &[&str],
        limit: usize,
    ) -> Result<(), RerankerError> {
        let _span = tracing::info_span!(
            "rerank",
            count = results.len(),
            limit,
            query_len = query.len()
        )
        .entered();
        if results.len() <= 1 {
            return Ok(());
        }
        if results.len() != passages.len() {
            // P3.11: structured warn so operators see the mismatch in journal,
            // and surface `InvalidArguments` instead of `Inference` so callers
            // can match on the caller-bug case distinctly from model errors.
            tracing::warn!(
                passages = passages.len(),
                results = results.len(),
                "rerank_with_passages: length mismatch — caller bug, refusing to score",
            );
            return Err(RerankerError::InvalidArguments(format!(
                "passages length ({}) must match results length ({})",
                passages.len(),
                results.len()
            )));
        }

        let Some(scores) = self.compute_scores_opt(query, passages)? else {
            return Ok(());
        };
        apply_rerank_scores(results, scores, limit);
        Ok(())
    }

    /// Clear the ONNX session to free memory (~91MB model).
    ///
    /// Session re-initializes lazily on next `rerank()` call.
    /// Use this during idle periods in long-running processes.
    fn clear_session(&self) {
        let mut guard = self.session.lock().unwrap_or_else(|p| p.into_inner());
        *guard = None;
        // Reset the input-shape probe so the next session re-detects
        // the loaded model's token_type_ids contract.
        *self
            .expects_token_type_ids
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = None;
        // RM-V1.25-15: Drop the tokenizer too (~20MB for ms-marco MiniLM).
        // In-flight rerank() calls that grabbed an Arc clone before this
        // call keep their own copy; the slot is cleared and lazy-reloads
        // on next tokenizer() access.
        let mut tok = self.tokenizer.lock().unwrap_or_else(|p| p.into_inner());
        *tok = None;
        tracing::info!("Reranker session and tokenizer cleared");
    }
}

/// EX-V1.30.1-8 (#1220): no-op pass-through reranker.
///
/// Returns `Ok(())` from every method — the input results are left
/// alone (their existing scores keep them ordered as they came in).
/// Used by the eval harness for ablation A/B (`--reranker none|onnx`)
/// to instantly compare the dense+SPLADE result quality against the
/// reranked version without a model load. Cheap to construct, no
/// model state, `clear_session` is a default no-op.
pub struct NoopReranker;

impl NoopReranker {
    pub fn new() -> Self {
        Self
    }
}

impl Default for NoopReranker {
    fn default() -> Self {
        Self::new()
    }
}

impl Reranker for NoopReranker {
    fn rerank(
        &self,
        _query: &str,
        results: &mut Vec<SearchResult>,
        limit: usize,
    ) -> Result<(), RerankerError> {
        // The contract of `rerank` is "score, sort, truncate". A
        // no-op skips the score+sort but should still truncate so
        // callers get the documented `len() <= limit` shape — same
        // way `OnnxReranker::rerank` ends with `apply_rerank_scores`
        // → `truncate`. Keep the existing input order: the dense leg
        // already sorted by cosine descending, so truncating after
        // gives the unranked baseline the eval harness wants to
        // compare against.
        if results.len() > limit {
            results.truncate(limit);
        }
        Ok(())
    }

    fn rerank_with_passages(
        &self,
        _query: &str,
        results: &mut Vec<SearchResult>,
        passages: &[&str],
        limit: usize,
    ) -> Result<(), RerankerError> {
        // Same length contract as the ONNX path: surface a length
        // mismatch loudly so a caller bug doesn't get masked by the
        // no-op shortcut.
        if results.len() != passages.len() {
            return Err(RerankerError::InvalidArguments(format!(
                "passages length ({}) must match results length ({})",
                passages.len(),
                results.len()
            )));
        }
        if results.len() > limit {
            results.truncate(limit);
        }
        Ok(())
    }
}

/// EX-V1.30.1-8 (#1220): LLM-judge reranker skeleton.
///
/// Holds an `Arc<dyn LlmRerankProvider>` so a future production deployment
/// can plug in a Claude / GPT / Gemini scorer without touching the search
/// path. **The current shipped impl is a skeleton** — it returns
/// `RerankerError::Inference("LlmReranker not yet implemented")` from
/// every score-producing call. The point is to prove the trait surface
/// supports an LLM-shaped impl: an `LlmRerankProvider` produces relevance
/// scores asynchronously via the existing `cqs::llm::BatchProvider`-style
/// trait, and the `Reranker` shim turns that into the synchronous
/// `rerank` shape the search path expects.
///
/// Wiring the production version is a follow-up: the `score` method
/// becomes a `tokio::block_on` of a batch-call to the LLM provider with
/// `(query, passage)` pairs, parses scores from the response, and feeds
/// them through `apply_rerank_scores`. The trait surface here doesn't
/// change.
// SCAFFOLD-ONLY (#1220): demoted to `pub(crate)` and gated behind `#[cfg(test)]`
// per CQ-V1.33.0-8 because every score call returns `Err`. The trait-surface
// pin lives in the `tests` module below. Promote back to `pub` (and re-export
// from `lib.rs`) when the LLM provider wiring lands.
#[cfg(test)]
pub(crate) struct LlmReranker;

#[cfg(test)]
impl LlmReranker {
    /// Construct a skeleton instance. The skeleton returns
    /// `RerankerError::Inference` on every score call so an integration
    /// test against the production search path can verify the trait
    /// hooks without LLM-credential setup.
    pub fn new() -> Self {
        Self
    }
}

#[cfg(test)]
impl Default for LlmReranker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
impl Reranker for LlmReranker {
    fn rerank(
        &self,
        _query: &str,
        _results: &mut Vec<SearchResult>,
        _limit: usize,
    ) -> Result<(), RerankerError> {
        Err(RerankerError::Inference(
            "LlmReranker is a skeleton — wire to a BatchProvider in a follow-up PR (#1220)"
                .to_string(),
        ))
    }

    fn rerank_with_passages(
        &self,
        _query: &str,
        _results: &mut Vec<SearchResult>,
        _passages: &[&str],
        _limit: usize,
    ) -> Result<(), RerankerError> {
        Err(RerankerError::Inference(
            "LlmReranker is a skeleton — wire to a BatchProvider in a follow-up PR (#1220)"
                .to_string(),
        ))
    }
}

#[cfg(test)]
mod batch_scaling_tests {
    use super::{
        reranker_batch_size, DEFAULT_RERANKER_BATCH, REFERENCE_HIDDEN_SIZE, REFERENCE_MAX_LENGTH,
    };

    /// Process-wide lock for the env-override tests so parallel cargo
    /// test runs don't observe each other's `CQS_RERANKER_BATCH` writes.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Reference shape (hidden=384, seq=512) reproduces the historic
    /// baseline 32 — existing operator setups see no behavior change.
    /// Pinning this catches a refactor that accidentally re-derives
    /// the rounded value.
    #[test]
    fn baseline_dims_return_default_batch() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::remove_var("CQS_RERANKER_BATCH");
        assert_eq!(
            reranker_batch_size(REFERENCE_MAX_LENGTH, REFERENCE_HIDDEN_SIZE),
            DEFAULT_RERANKER_BATCH
        );
    }

    /// SHL-V1.36-6 motivating case: a custom 2048-token reranker at
    /// the baseline width. seq_factor = 512/2048 = 0.25, hidden_factor =
    /// 1.0; batch = 32 * 0.25 * 1.0 = 8.
    #[test]
    fn long_seq_reduces_batch() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::remove_var("CQS_RERANKER_BATCH");
        assert_eq!(reranker_batch_size(2048, REFERENCE_HIDDEN_SIZE), 8);
    }

    /// 8K-seq rerankers shouldn't degenerate to batch=1. The 0.25 floor
    /// in `seq_factor` keeps batch ≥ 8 at the reference hidden_size.
    #[test]
    fn extreme_seq_floors_at_quarter() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::remove_var("CQS_RERANKER_BATCH");
        assert_eq!(reranker_batch_size(8192, REFERENCE_HIDDEN_SIZE), 8);
        assert_eq!(reranker_batch_size(32_768, REFERENCE_HIDDEN_SIZE), 8);
    }

    /// Short-seq rerankers (e.g., 256-token cross-encoders) at baseline
    /// width get a wider batch — `512 / 256 = 2.0` so 32 → 64.
    #[test]
    fn short_seq_widens_batch() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::remove_var("CQS_RERANKER_BATCH");
        assert_eq!(reranker_batch_size(256, REFERENCE_HIDDEN_SIZE), 64);
        assert_eq!(reranker_batch_size(128, REFERENCE_HIDDEN_SIZE), 128);
    }

    /// hidden_size = 768 (e.g. MiniLM-L-12 / many cross-encoders) at
    /// baseline seq halves the batch. seq_factor = 1.0, hidden_factor =
    /// 384/768 = 0.5; batch = 32 * 1.0 * 0.5 = 16.
    #[test]
    fn wider_hidden_halves_batch() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::remove_var("CQS_RERANKER_BATCH");
        assert_eq!(reranker_batch_size(REFERENCE_MAX_LENGTH, 768), 16);
    }

    /// hidden_size = 1024 (BERT-large class) at baseline seq quarters
    /// the batch. seq_factor = 1.0, hidden_factor = 384/1024 ≈ 0.375;
    /// scaled = 32 * 0.375 = 12; rounded = 16 (next power of 2).
    #[test]
    fn bert_large_hidden_quarters_batch() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::remove_var("CQS_RERANKER_BATCH");
        assert_eq!(reranker_batch_size(REFERENCE_MAX_LENGTH, 1024), 16);
    }

    /// Compound case: a 768-hidden reranker running at 2048 seq.
    /// seq_factor = 0.25, hidden_factor = 0.5; batch = 32 * 0.25 * 0.5 = 4.
    /// Both factors carry their share of the OOM mitigation.
    #[test]
    fn wide_long_compound_reduces_batch() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::remove_var("CQS_RERANKER_BATCH");
        assert_eq!(reranker_batch_size(2048, 768), 4);
    }

    /// Net floor at extreme model: 0.25 (seq) × 0.25 (hidden) → 32 × 0.0625
    /// = 2 (next_power_of_two of `2.0_f64 as usize` = 2). The clamp ensures
    /// we never return 0 or 1 even on absurd model dims.
    #[test]
    fn extreme_compound_floors_at_two() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::remove_var("CQS_RERANKER_BATCH");
        assert_eq!(reranker_batch_size(32_768, 8192), 2);
    }

    /// `CQS_RERANKER_BATCH` env wins regardless of (hidden, seq).
    /// Operators pinning a value know their own workload — we don't
    /// second-guess them.
    #[test]
    fn env_override_wins_regardless_of_dims() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("CQS_RERANKER_BATCH", "100");
        assert_eq!(reranker_batch_size(2048, 768), 100);
        assert_eq!(reranker_batch_size(512, 384), 100);
        std::env::remove_var("CQS_RERANKER_BATCH");
    }

    /// Degenerate inputs (`hidden_size=0`, `max_length=0`) don't panic.
    /// The `.max(1)` floors on both dims keep the formula safe.
    #[test]
    fn zero_dims_do_not_panic() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::remove_var("CQS_RERANKER_BATCH");
        let _ = reranker_batch_size(0, REFERENCE_HIDDEN_SIZE);
        let _ = reranker_batch_size(REFERENCE_MAX_LENGTH, 0);
        let _ = reranker_batch_size(0, 0);
    }
}

/// Verify file checksum using blake3
fn verify_checksum(path: &std::path::Path, expected: &str) -> Result<(), RerankerError> {
    let mut file = std::fs::File::open(path).map_err(|e| {
        RerankerError::ModelDownload(format!("Cannot open {}: {}", path.display(), e))
    })?;
    let mut hasher = blake3::Hasher::new();
    std::io::copy(&mut file, &mut hasher).map_err(|e| {
        RerankerError::ModelDownload(format!("Read error on {}: {}", path.display(), e))
    })?;
    let actual = hasher.finalize().to_hex().to_string();

    if actual != expected {
        return Err(RerankerError::ChecksumMismatch {
            path: path.display().to_string(),
            expected: expected.to_string(),
            actual,
        });
    }
    Ok(())
}

/// Computes the sigmoid activation function.
///
/// The sigmoid function maps any input value to a range between 0 and 1, making it useful for neural networks and probability calculations. It is defined as 1 / (1 + e^(-x)).
///
/// # Arguments
///
/// * `x` - The input value
///
/// # Returns
///
/// The sigmoid of x, a value in the range (0, 1)
fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

/// Apply freshly computed cross-encoder scores, then sort and truncate.
///
/// When `scores` is empty, leaves `results` unchanged (used for the degenerate
/// paths in [`Reranker::rerank`]). Otherwise, writes each score onto the
/// corresponding result, sorts descending with a chunk-id secondary key for
/// deterministic tie-breaking, and truncates to `limit`.
///
/// PF-V1.25-5: extracted from the impl block so the &mut results write and
/// the earlier &results passage borrow live in disjoint scopes at the call
/// site (`rerank`).
fn apply_rerank_scores(results: &mut Vec<SearchResult>, scores: Vec<f32>, limit: usize) {
    if scores.is_empty() {
        return;
    }
    let n = scores.len().min(results.len());
    // AC-V1.33-9: if the reranker returned fewer scores than results
    // (`compute_scores_opt` currently can't, but a future ONNX backend that
    // short-circuits on a per-batch failure could), drop the un-rescored
    // tail rather than mixing cross-encoder cohort scores ([0, 1] post
    // sigmoid) with the surviving cosine cohort ([-1, 1]) inside the same
    // sort comparator. The mixed comparison would interleave the two
    // cohorts arbitrarily, producing neither pure rerank nor pure semantic
    // ranking. Truncating the un-rescored tail keeps the surviving cohort
    // homogeneous.
    if n < results.len() {
        tracing::warn!(
            scores = scores.len(),
            results = results.len(),
            "Reranker returned fewer scores than results; dropping un-rescored tail to keep cohort homogeneous"
        );
        results.truncate(n);
    }
    for (i, score) in scores.into_iter().take(n).enumerate() {
        results[i].score = score;
    }
    let batch_size = results.len();
    // 5. Sort descending by score, truncate. Secondary sort on chunk id keeps
    // equal-score candidates deterministically ordered so the truncate()
    // drops the same candidates on every invocation.
    results.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(a.chunk.id.cmp(&b.chunk.id))
    });
    results.truncate(limit);
    tracing::info!(reranked = results.len(), batch_size, "Re-ranking complete");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sigmoid_zero() {
        let result = sigmoid(0.0);
        assert!((result - 0.5).abs() < 1e-6);
    }

    #[test]
    fn test_sigmoid_large_positive() {
        let result = sigmoid(10.0);
        assert!(result > 0.999);
    }

    #[test]
    fn test_sigmoid_large_negative() {
        let result = sigmoid(-10.0);
        assert!(result < 0.001);
    }

    #[test]
    fn test_sigmoid_extreme_negative() {
        // Should not panic or produce NaN
        let result = sigmoid(-100.0);
        assert!(result >= 0.0 && result.is_finite());
    }

    #[test]
    fn test_sigmoid_nan_does_not_panic() {
        // TC-1: If the model returns NaN logits, sigmoid should not panic.
        // NaN propagates through arithmetic, producing NaN output.
        // The reranker's total_cmp sort handles NaN (sorts to end).
        let result = sigmoid(f32::NAN);
        assert!(result.is_nan(), "sigmoid(NaN) should be NaN, got {result}");
    }

    #[test]
    fn test_sigmoid_infinity_does_not_panic() {
        let pos = sigmoid(f32::INFINITY);
        assert!(
            pos.is_finite() || pos.is_nan(),
            "sigmoid(+inf) should not panic"
        );
        let neg = sigmoid(f32::NEG_INFINITY);
        assert!(
            neg.is_finite() || neg.is_nan(),
            "sigmoid(-inf) should not panic"
        );
    }

    #[test]
    fn test_reranker_new() {
        // Construction should succeed (no model download yet — lazy)
        let reranker = OnnxReranker::new();
        assert!(reranker.is_ok());
    }

    #[test]
    fn test_rerank_empty_results() {
        let reranker = OnnxReranker::new().unwrap();
        let mut results = Vec::new();
        let result = reranker.rerank("test query", &mut results, 10);
        assert!(result.is_ok());
        assert!(results.is_empty());
    }

    // TC-HAP-1.29-3: happy-path + empty-input pins for `rerank` /
    // `rerank_with_passages`.
    //
    // The `#[ignore]` test loads the ms-marco-MiniLM-L-6-v2 model on first
    // use (~91 MB ONNX, one-time HF fetch), so it is opt-in. Run with:
    //   cargo test --features gpu-index --lib reranker::tests -- --ignored
    // The non-ignored counterpart exercises the empty-input shortcut for
    // `rerank_with_passages` — no model load, runs on every PR.

    use crate::parser::{ChunkType, Language};
    use crate::store::ChunkSummary;
    use std::path::PathBuf;

    /// Build a minimal SearchResult whose `content` is the passage to score.
    /// The rest of the ChunkSummary is filler — the reranker only looks at
    /// `chunk.content` (for `rerank`) or the externally supplied passages
    /// (for `rerank_with_passages`). `apply_rerank_scores` uses `chunk.id`
    /// as the tie-break key, so give each stub a unique id.
    fn stub_result(id: &str, content: &str) -> SearchResult {
        let hash = blake3::hash(content.as_bytes()).to_hex().to_string();
        let chunk = ChunkSummary {
            id: id.to_string(),
            file: PathBuf::from("test.rs"),
            language: Language::Rust,
            chunk_type: ChunkType::Function,
            name: id.to_string(),
            signature: format!("fn {id}()"),
            content: content.to_string(),
            doc: None,
            line_start: 1,
            line_end: 5,
            content_hash: hash,
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
            vendored: false,
        };
        SearchResult { chunk, score: 0.0 }
    }

    /// TC-HAP-1.29-3a: seed three passages where the "baking sourdough" one
    /// is clearly irrelevant to a Rust-async query. After rerank, the baking
    /// passage must sort last. Gated `#[ignore]` because the first call
    /// cold-loads the ms-marco MiniLM model (~91 MB ONNX).
    #[test]
    #[ignore = "loads cross-encoder model; run with --ignored"]
    fn test_rerank_reorders_by_relevance() {
        let reranker = match OnnxReranker::new() {
            Ok(r) => r,
            Err(e) => {
                eprintln!("ms-marco-MiniLM unavailable in test env: {e}; skipping (#1305)");
                return;
            }
        };
        // Order at input is intentionally NOT relevance order — we want to
        // see that the reranker does the work, not that it preserves input
        // ordering by accident.
        let mut results = vec![
            stub_result(
                "bake",
                "A step-by-step guide to bake sourdough bread at home.",
            ),
            stub_result(
                "tokio",
                "Tokio is an asynchronous runtime for Rust. It provides an \
                 event loop and async APIs for network I/O.",
            ),
            stub_result(
                "futures",
                "The Future trait in Rust represents a value that may not be \
                 ready yet. Use .await to drive it to completion on an async \
                 runtime.",
            ),
        ];

        reranker
            .rerank("rust async await", &mut results, 3)
            .expect("rerank");

        assert_eq!(results.len(), 3, "rerank must preserve the full input set");
        let last = results.last().expect("non-empty");
        assert_eq!(
            last.chunk.id,
            "bake",
            "baking-sourdough passage must rank last for a rust-async query. \
             got order: {:?}",
            results
                .iter()
                .map(|r| r.chunk.id.as_str())
                .collect::<Vec<_>>()
        );
        // Sanity: all three original chunk ids must still be present.
        let ids: std::collections::HashSet<&str> =
            results.iter().map(|r| r.chunk.id.as_str()).collect();
        assert!(ids.contains("bake"));
        assert!(ids.contains("tokio"));
        assert!(ids.contains("futures"));
    }

    /// TC-HAP-1.29-3b: `rerank_with_passages` on empty input is a no-op —
    /// the `results.len() <= 1` shortcut at line 214 fires before any model
    /// load, so this test runs without the ONNX session.
    #[test]
    fn test_rerank_empty_input_returns_empty() {
        let reranker = OnnxReranker::new().expect("reranker new");
        let mut results: Vec<SearchResult> = Vec::new();
        let passages: Vec<&str> = Vec::new();
        reranker
            .rerank_with_passages("anything", &mut results, &passages, 10)
            .expect("rerank_with_passages on empty input must be ok");
        assert!(
            results.is_empty(),
            "empty input → empty output (no-op shortcut)"
        );
    }

    /// EX-V1.30.1-8 (#1220): `NoopReranker::rerank` truncates to `limit`
    /// and preserves input order otherwise. Pins the contract that the
    /// eval-harness ablation switch (`--reranker none`) gives an
    /// apples-to-apples baseline against the ONNX path: both end with
    /// `results.len() <= limit`, only the order differs.
    #[test]
    fn noop_reranker_truncates_but_preserves_order() {
        let reranker = NoopReranker::new();
        let mut results = vec![
            stub_result("a", "first"),
            stub_result("b", "second"),
            stub_result("c", "third"),
            stub_result("d", "fourth"),
        ];
        // Set distinct scores so we can detect any unwanted re-sort.
        for (i, r) in results.iter_mut().enumerate() {
            r.score = (i as f32) * 0.25; // 0.0, 0.25, 0.5, 0.75
        }

        reranker
            .rerank("any query", &mut results, 2)
            .expect("noop rerank");

        assert_eq!(results.len(), 2, "truncated to limit");
        assert_eq!(
            results[0].chunk.id, "a",
            "no re-sort: first input position survives"
        );
        assert_eq!(
            results[1].chunk.id, "b",
            "no re-sort: second input position survives"
        );
    }

    /// `NoopReranker::rerank_with_passages` enforces the same length
    /// contract as the ONNX path so a caller bug doesn't get masked by
    /// the no-op shortcut.
    #[test]
    fn noop_reranker_rejects_passage_length_mismatch() {
        let reranker = NoopReranker::new();
        let mut results = vec![stub_result("a", "x"), stub_result("b", "y")];
        let passages = ["only one"];
        let err = reranker
            .rerank_with_passages("q", &mut results, &passages, 10)
            .unwrap_err();
        assert!(
            matches!(err, RerankerError::InvalidArguments(_)),
            "expected InvalidArguments on length mismatch, got {err:?}"
        );
    }

    /// TC-ADV-V1.33-4: pin that the length-mismatch error message contains
    /// both lengths. P3.11 surfaced this as `InvalidArguments` so callers
    /// can pattern-match the caller-bug case distinctly from model errors;
    /// a future refactor that flipped the variant or dropped the lengths
    /// from the message would break operator log-grep loops.
    #[test]
    fn test_rerank_with_passages_length_mismatch_returns_invalid_arguments() {
        let reranker = NoopReranker::new();
        // Three results, two passages — mismatch.
        let mut results = vec![
            stub_result("a", "first"),
            stub_result("b", "second"),
            stub_result("c", "third"),
        ];
        let passages = ["first passage", "second passage"];
        let err = reranker
            .rerank_with_passages("q", &mut results, &passages, 10)
            .expect_err("length mismatch must error");
        let msg = match err {
            RerankerError::InvalidArguments(s) => s,
            other => panic!("expected InvalidArguments, got {other:?}"),
        };
        assert!(
            msg.contains("3"),
            "error message must mention results.len()=3, got: {msg:?}"
        );
        assert!(
            msg.contains("2"),
            "error message must mention passages.len()=2, got: {msg:?}"
        );
    }

    /// AC-V1.33-9: when a reranker backend returns fewer scores than results
    /// (a future per-batch-failure short-circuit), `apply_rerank_scores`
    /// must NOT mix cross-encoder cohort scores ([0, 1] post-sigmoid) with
    /// the surviving cosine cohort ([-1, 1]) — the partial-overwrite path.
    /// Truncating the un-rescored tail is the contract: every survivor in
    /// the output must have come from the rerank cohort, never from the
    /// pre-rerank cosine cohort.
    #[test]
    fn apply_rerank_scores_drops_unrescored_tail_on_length_mismatch() {
        // Five results, only three scores. The last two carry pre-rerank
        // cosine-style scores (here: 0.99) that, if mixed with the
        // sigmoid-mapped rerank scores ([0, 1]), would arbitrarily
        // outrank the rerank survivors.
        let mut results = vec![
            stub_result("a", "first"),
            stub_result("b", "second"),
            stub_result("c", "third"),
            stub_result("d", "fourth"),
            stub_result("e", "fifth"),
        ];
        // Pre-rerank: tail has high cosine scores that should NOT survive.
        results[3].score = 0.99;
        results[4].score = 0.95;
        // Rerank cohort: low sigmoid scores that, before this fix, would
        // have lost the sort to the cosine tail.
        let scores = vec![0.1f32, 0.05, 0.2];
        super::apply_rerank_scores(&mut results, scores, 10);
        assert_eq!(
            results.len(),
            3,
            "un-rescored tail must be dropped (5 results -> 3 after truncate to scores.len())"
        );
        let survivors: std::collections::HashSet<&str> =
            results.iter().map(|r| r.chunk.id.as_str()).collect();
        assert!(survivors.contains("a"), "rescored a must survive");
        assert!(survivors.contains("b"), "rescored b must survive");
        assert!(survivors.contains("c"), "rescored c must survive");
        assert!(
            !survivors.contains("d") && !survivors.contains("e"),
            "un-rescored cosine cohort must be dropped, got {survivors:?}"
        );
        // Every surviving score must be from the rerank cohort (<= 0.2),
        // never from the cosine tail (>= 0.95) — the cohort-mixing bug.
        for r in &results {
            assert!(
                r.score <= 0.2 + f32::EPSILON,
                "survivor {} carries score {} — must be from rerank cohort, not cosine tail",
                r.chunk.id,
                r.score
            );
        }
    }

    /// `LlmReranker` is a skeleton — every score-producing call returns
    /// `RerankerError::Inference` so an integration test against the
    /// production search path can verify trait wiring without any
    /// live LLM credentials. Pins that contract: any future production
    /// `LlmReranker` impl must NOT silently no-op when the provider is
    /// unconfigured.
    #[test]
    fn llm_reranker_skeleton_returns_inference_error() {
        let reranker = LlmReranker::new();
        let mut results = vec![stub_result("a", "x"), stub_result("b", "y")];
        let err = reranker.rerank("q", &mut results, 10).unwrap_err();
        let RerankerError::Inference(msg) = err else {
            panic!("expected Inference variant, got {err:?}");
        };
        assert!(
            msg.contains("skeleton"),
            "skeleton error must self-identify; got: {msg}"
        );
    }

    // ===== TC-HAP-V1.33-10: resolve_reranker config-path coverage =====
    //
    // P1.7 fix shipped `OnnxReranker::with_section(section: Option<...>)`
    // so `.cqs.toml` `[reranker]` `preset` / `model_path` / `tokenizer_path`
    // override the hardcoded default. These tests pin the precedence chain
    // documented at line 57-58 ("CLI → CQS_RERANKER_MODEL → [reranker]
    // model_path → [reranker] preset → hardcoded ms-marco-minilm").
    //
    // Pure-config — no ONNX runtime needed, just `aux_model::resolve`
    // dispatch. CQS_RERANKER_MODEL must be unset for deterministic
    // results, hence the cross-test lock.

    use std::sync::Mutex;
    static RERANKER_ENV_LOCK: Mutex<()> = Mutex::new(());

    /// TC-HAP-V1.33-10: `[reranker] preset = "ms-marco-minilm"` resolves
    /// to the canonical preset config. Pins the preset branch (line 5 of
    /// the precedence chain documented at reranker.rs:57-58).
    #[test]
    fn resolve_reranker_with_preset_resolves_to_preset_path() {
        let _guard = RERANKER_ENV_LOCK.lock().unwrap();
        let prev = std::env::var("CQS_RERANKER_MODEL").ok();
        std::env::remove_var("CQS_RERANKER_MODEL");

        let section = AuxModelSection {
            preset: Some("ms-marco-minilm".to_string()),
            model_path: None,
            tokenizer_path: None,
        };
        let cfg = resolve_reranker(Some(&section)).expect("resolve must succeed for preset");

        // Restore env before any assertion so a panic doesn't leak state.
        match prev {
            Some(v) => std::env::set_var("CQS_RERANKER_MODEL", v),
            None => std::env::remove_var("CQS_RERANKER_MODEL"),
        }

        assert_eq!(
            cfg.preset.as_deref(),
            Some("ms-marco-minilm"),
            "preset name must round-trip through resolve"
        );
        assert_eq!(
            cfg.repo.as_deref(),
            Some("cross-encoder/ms-marco-MiniLM-L-6-v2"),
            "ms-marco-minilm preset must point at the cross-encoder repo"
        );
    }

    /// TC-HAP-V1.33-10: when no `[reranker]` section is provided,
    /// `resolve_reranker` falls back to the hardcoded default preset
    /// (`ms-marco-minilm`). Pins the last branch of the precedence chain.
    #[test]
    fn resolve_reranker_with_no_section_falls_back_to_default_preset() {
        let _guard = RERANKER_ENV_LOCK.lock().unwrap();
        let prev = std::env::var("CQS_RERANKER_MODEL").ok();
        std::env::remove_var("CQS_RERANKER_MODEL");

        let cfg = resolve_reranker(None).expect("default-preset fallback must succeed");

        match prev {
            Some(v) => std::env::set_var("CQS_RERANKER_MODEL", v),
            None => std::env::remove_var("CQS_RERANKER_MODEL"),
        }

        assert_eq!(
            cfg.preset.as_deref(),
            Some("ms-marco-minilm"),
            "default fallback must be ms-marco-minilm"
        );
    }

    /// TC-HAP-V1.33-10: model-loading variant — covers the full
    /// `OnnxReranker::with_section` construction path. Gated `#[ignore]`
    /// because the lazy session load that follows construction needs the
    /// real ONNX model, but with_section itself only stores the section
    /// (no model download). Runs in ci-slow.yml's full-suite job.
    #[test]
    #[ignore = "construction-only smoke; full reranker uses ms-marco-MiniLM model from ~/.cache/huggingface; runs in ci-slow.yml full-suite job"]
    fn test_onnx_reranker_with_section() {
        let section = AuxModelSection {
            preset: Some("ms-marco-minilm".to_string()),
            model_path: None,
            tokenizer_path: None,
        };
        let reranker = OnnxReranker::with_section(Some(section.clone()))
            .expect("with_section must construct without model load");
        // The cached section is stored for lazy `model_paths` resolution.
        // Sanity-check: the field is populated.
        assert!(
            reranker.section.is_some(),
            "with_section must store the section for lazy resolve_reranker"
        );
    }
}
