//! Embedding model configuration: presets, resolution, config-file parsing.

use serde::Deserialize;

// ---------------------------------------------------------------------------
// Default tensor name helpers (used by serde defaults on `InputNames`).
// ---------------------------------------------------------------------------

fn default_ids_name() -> String {
    "input_ids".to_string()
}
fn default_mask_name() -> String {
    "attention_mask".to_string()
}
fn default_output_name() -> String {
    "last_hidden_state".to_string()
}

/// Names of the input tensors consumed by the ONNX model.
///
/// Most BERT-family embedders use the triple `(input_ids, attention_mask,
/// token_type_ids)`. Some distilled or non-BERT models drop `token_type_ids`
/// or rename the tensors entirely. This struct makes those names configurable
/// instead of hard-coding them in the encoder.
///
/// # Defaults
/// - `ids`: `"input_ids"`
/// - `mask`: `"attention_mask"`
/// - `token_types`: `None` — set to `Some("token_type_ids")` for BERT.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct InputNames {
    /// Name of the token-id tensor (default `"input_ids"`).
    #[serde(default = "default_ids_name")]
    pub ids: String,
    /// Name of the attention-mask tensor (default `"attention_mask"`).
    #[serde(default = "default_mask_name")]
    pub mask: String,
    /// Name of the token-type-id tensor, if the model consumes it.
    /// `None` means the input is not supplied to `session.run`.
    #[serde(default)]
    pub token_types: Option<String>,
}

impl Default for InputNames {
    /// Standard BERT input names: `input_ids`, `attention_mask`, `token_type_ids`.
    ///
    /// Matches BGE-large, E5-base, and v9-200k presets.
    fn default() -> Self {
        Self::bert()
    }
}

impl InputNames {
    /// Standard BERT input names: `input_ids`, `attention_mask`, `token_type_ids`.
    ///
    /// Used by BGE-large, E5-base, and v9-200k.
    pub fn bert() -> Self {
        Self {
            ids: default_ids_name(),
            mask: default_mask_name(),
            token_types: Some("token_type_ids".to_string()),
        }
    }

    /// BERT-like inputs without `token_type_ids`.
    ///
    /// Used by some distilled variants and non-BERT transformers (e.g. Jina v2,
    /// models that dropped segment embeddings during distillation).
    pub fn bert_no_token_types() -> Self {
        Self {
            ids: default_ids_name(),
            mask: default_mask_name(),
            token_types: None,
        }
    }
}

/// Strategy for reducing the per-token hidden states to a single vector.
///
/// The encoder dispatches on this after running `session.run`. All strategies
/// preserve the hidden dimension; downstream L2-normalization happens in
/// [`normalize_l2`][crate::embedder] regardless of choice.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum PoolingStrategy {
    /// Mean-pool the masked token positions. **Current default** — BGE, E5, v9-200k.
    #[default]
    Mean,
    /// Use the first-token (`[CLS]`) embedding directly.
    ///
    /// Some DistilBERT-derived embedders are trained for CLS pooling; using
    /// mean pooling on them degrades quality silently.
    Cls,
    /// Use the last non-padding token, selected via the attention mask.
    ///
    /// Used by autoregressive / decoder-only embedders (rare: Qwen3-Embedding,
    /// some Mistral-based embedders).
    LastToken,
    /// The ONNX output is already pooled — return it as-is, after L2 norm.
    ///
    /// Used by models whose ONNX export includes a task-aware pooling head
    /// that cannot be reproduced by mean/cls/last over the per-token hidden
    /// state. Example: EmbeddingGemma's `sentence_embedding` output is
    /// computed by an internal projection layer; mean-pooling
    /// `last_hidden_state` produces a vector that has cosine ≈ 0 with the
    /// model's own pooled output (verified empirically — this isn't a
    /// missing-attention-mask issue, it's a fundamentally different head).
    /// The strategy expects a 2D `[batch, dim]` output tensor; the embed
    /// loop skips the 3D reshape + pool dispatch and emits the rows directly.
    Identity,
}

/// Configuration for an embedding model.
///
/// Defines everything needed to download, load, and use an ONNX embedding model:
/// repository location, file paths, dimensions, text prefixes, and the
/// architecture-specific I/O contract (input tensor names, output tensor name,
/// pooling strategy).
#[derive(Debug, Clone)]
pub struct ModelConfig {
    /// Short human-readable name (e.g. "e5-base", "bge-large")
    pub name: String,
    /// HuggingFace repo ID (e.g. "intfloat/e5-base-v2")
    pub repo: String,
    /// Path to ONNX model file within the HuggingFace repo (always forward-slash separated,
    /// e.g., `"onnx/model.onnx"`). Not a filesystem path -- HuggingFace Hub resolves this.
    pub onnx_path: String,
    /// Path to tokenizer file within the repo
    pub tokenizer_path: String,
    /// Embedding dimension (1024 for BGE-large, 768 for E5-base)
    pub dim: usize,
    /// Maximum input sequence length in tokens
    pub max_seq_length: usize,
    /// Prefix prepended to queries (e.g. "query: " for E5)
    pub query_prefix: String,
    /// Prefix prepended to documents (e.g. "passage: " for E5)
    pub doc_prefix: String,
    /// Names of the input tensors the ONNX model consumes.
    ///
    /// Defaults to standard BERT: `input_ids` + `attention_mask` + `token_type_ids`.
    pub input_names: InputNames,
    /// Name of the output tensor to pool over (most models: `"last_hidden_state"`;
    /// sentence-transformers-packaged models sometimes expose `"sentence_embedding"`).
    pub output_name: String,
    /// How to reduce per-token hidden states to a single embedding vector.
    ///
    /// Defaults to [`PoolingStrategy::Mean`] (BGE, E5, v9-200k).
    pub pooling: PoolingStrategy,
    /// EX-V1.29-6: Approximate download size of the ONNX bundle in bytes.
    ///
    /// Populated for shipped presets so `cqs init` can report a concrete size
    /// instead of the old `dim >= 1024 ? "~1.3GB" : "~547MB"` heuristic
    /// (which silently misreported custom models and smaller-dim BGE variants).
    /// `None` when unknown — the init command falls back to "(size unknown)"
    /// rather than guessing. Custom models from `EmbeddingConfig` leave this
    /// unset; operators supplying a custom repo know their own sizes.
    pub approx_download_bytes: Option<u64>,
    /// SHL-V1.29-1: Token id used to pad `input_ids` / `attention_mask` tensors
    /// below `max_length`.
    ///
    /// Every BERT/E5/BGE variant cqs ships today uses `0` (the `[PAD]` token);
    /// this has been hardcoded at `pad_2d_i64(..., 0)` call sites since the
    /// encoder's first draft. A custom ONNX export with a different pad id
    /// silently gets wrong padding — the model still runs (the attention
    /// mask zeros the positions) but the pre-mask hidden states are
    /// unambiguously different, which leaks into mean-pooled embeddings when
    /// the mask shape leaves a spurious `1`. The encoder now reads the
    /// tokenizer's declared pad id at session init (see `Embedder::pad_id`)
    /// with this field as the fallback when the tokenizer omits a pad
    /// configuration.
    pub pad_id: i64,
}

// ---------------------------------------------------------------------------
// Macro: define_embedder_presets!
//
// Generates from a single declaration table, mirroring `define_languages!` /
// `define_chunk_types!` / `define_query_categories!`:
//   - One `pub fn <ctor>(&self) -> Self` constructor per preset on `ModelConfig`
//   - `ModelConfig::PRESET_NAMES: &'static [&'static str]` (short names)
//   - `ModelConfig::from_preset(name)` matching short-name OR repo ID
//   - `ModelConfig::default_model()` returning the row marked `default = true`
//
// Adding a preset = one new line here. The standalone `DEFAULT_MODEL_REPO`
// and `DEFAULT_DIM` constants disappear — `ModelInfo::default()` and
// `with_dim()` derive from `default_model()` directly.
// ---------------------------------------------------------------------------
/// Defines the embedder-preset table.
///
/// Each row carries:
/// - `$variant_fn`: snake_case constructor name on `ModelConfig` (e.g., `bge_large`)
/// - `$name`: short name (e.g., `"bge-large"`) — also a `from_preset` key
/// - `$repo`: HuggingFace repo ID (also a `from_preset` key)
/// - `$onnx_path`, `$tokenizer_path`, `$dim`, `$max_seq_length`
/// - `$query_prefix`, `$doc_prefix`
/// - `$input_names`, `$output_name`, `$pooling` — architecture knobs
/// - Optional `default` marker — exactly one row may set this
macro_rules! define_embedder_presets {
    (
        $(
            $(#[doc = $doc:expr])*
            $variant_fn:ident => name = $name:literal, repo = $repo:literal, onnx_path = $onnx_path:literal,
                tokenizer_path = $tokenizer_path:literal, dim = $dim:literal, max_seq_length = $max:literal,
                query_prefix = $qp:literal, doc_prefix = $dp:literal,
                input_names = $input_names:expr, output_name = $output_name:expr, pooling = $pooling:expr,
                approx_download_bytes = $bytes:expr, pad_id = $pad_id:literal
                $(, default = $default:tt)?
                ;
        )+
    ) => {
        impl ModelConfig {
            $(
                $(#[doc = $doc])*
                pub fn $variant_fn() -> Self {
                    Self {
                        name: $name.to_string(),
                        repo: $repo.to_string(),
                        onnx_path: $onnx_path.to_string(),
                        tokenizer_path: $tokenizer_path.to_string(),
                        dim: $dim,
                        max_seq_length: $max,
                        query_prefix: $qp.to_string(),
                        doc_prefix: $dp.to_string(),
                        input_names: $input_names,
                        output_name: $output_name,
                        pooling: $pooling,
                        approx_download_bytes: $bytes,
                        pad_id: $pad_id,
                    }
                }
            )+

            /// All preset short names, in declaration order.
            pub const PRESET_NAMES: &'static [&'static str] = &[ $($name),+ ];

            /// All preset repo IDs, in declaration order.
            #[allow(dead_code)]
            pub const PRESET_REPOS: &'static [&'static str] = &[ $($repo),+ ];

            /// Default model repo as a `&'static str` for compile-time
            /// metadata. Sourced from the row marked `default = true` in
            /// `define_embedder_presets!`. Use `default_model().repo` when
            /// a `String` is acceptable.
            pub const DEFAULT_REPO: &'static str = {
                // Compile-time selector — only the default-marked row
                // contributes a `Some(repo)`; all others contribute `None`.
                // The trailing `match … None => panic!` is a compile-time
                // guard against forgetting to mark a default in the table.
                #[allow(unused_assignments)]
                let mut r: Option<&'static str> = None;
                $(
                    r = define_embedder_presets!(@default_const r, $repo $(, $default)?);
                )+
                match r {
                    Some(s) => s,
                    None => panic!("define_embedder_presets!: no row marked `default = true`"),
                }
            };

            /// Default embedding dimension as a `usize` for compile-time
            /// array sizing and `pub const` consumers (e.g. `EMBEDDING_DIM`
            /// in `lib.rs`). Sourced from the row marked `default = true`.
            pub const DEFAULT_DIM: usize = {
                #[allow(unused_assignments)]
                let mut d: Option<usize> = None;
                $(
                    d = define_embedder_presets!(@default_const d, $dim $(, $default)?);
                )+
                match d {
                    Some(v) => v,
                    None => panic!("define_embedder_presets!: no row marked `default = true`"),
                }
            };

            /// Look up a preset by short name OR HuggingFace repo ID.
            ///
            /// Returns `None` for unknown names.
            pub fn from_preset(name: &str) -> Option<Self> {
                match name {
                    $(
                        $name | $repo => Some(Self::$variant_fn()),
                    )+
                    _ => None,
                }
            }

            /// The project default model. Single source of truth for all
            /// fallback paths — change the `default = true` marker in the
            /// table to switch the default for the entire project.
            ///
            /// Generated by `define_embedder_presets!`. Returns the row
            /// marked `default = true` (exactly one).
            pub fn default_model() -> Self {
                // Each row expands to either a `return` statement (the row
                // marked `default = true`) or to nothing. Compilation fails
                // if no row is marked default — the function would have no
                // return, triggering the "expected `Self`, found `()`"
                // mismatch on the trailing expression below.
                $(
                    define_embedder_presets!(@default_arm $variant_fn $(, $default)?);
                )+
                // Unreachable when exactly one row is marked default; this
                // line catches the all-`@nodefault` configuration at compile
                // time as a type error rather than a panic at run time.
                #[allow(unreachable_code)]
                {
                    panic!("define_embedder_presets!: no row marked `default = true`")
                }
            }
        }
    };

    // Internal: emits the function body for the row marked `default = true`.
    // Other rows expand to nothing.
    (@default_arm $variant_fn:ident, true) => { return Self::$variant_fn() };
    (@default_arm $variant_fn:ident) => { };

    // Internal: pick the value of the default-marked row.
    // The default-marked row replaces the previous binding with `Some(value)`;
    // other rows pass through unchanged.
    (@default_const $prev:ident, $value:literal, true) => { Some($value) };
    (@default_const $prev:ident, $value:literal) => { $prev };
}

define_embedder_presets! {
    /// E5-base-v2: 768-dim, 512 tokens. Lightweight preset.
    ///
    /// Standard BERT I/O (`input_ids` / `attention_mask` / `token_type_ids`),
    /// output `last_hidden_state`, mean pooling over the attention mask.
    e5_base => name = "e5-base", repo = "intfloat/e5-base-v2",
        onnx_path = "onnx/model.onnx", tokenizer_path = "tokenizer.json",
        dim = 768, max_seq_length = 512,
        query_prefix = "query: ", doc_prefix = "passage: ",
        input_names = InputNames::bert(), output_name = default_output_name(), pooling = PoolingStrategy::Mean,
        // EX-V1.29-6: ONNX bundle (model.onnx + tokenizer.json). ~547 MiB real.
        approx_download_bytes = Some(547 * 1024 * 1024),
        // SHL-V1.29-1: BERT [PAD] token = id 0.
        pad_id = 0;

    /// v9-200k LoRA: E5-base fine-tuned with call-graph false-negative filtering.
    /// 768-dim, 512 tokens. 90.5% R@1 on expanded eval (296 queries, 7 languages).
    ///
    /// Same architecture as E5-base: standard BERT I/O, mean pooling.
    v9_200k => name = "v9-200k", repo = "jamie8johnson/e5-base-v2-code-search",
        onnx_path = "model.onnx", tokenizer_path = "tokenizer.json",
        dim = 768, max_seq_length = 512,
        query_prefix = "query: ", doc_prefix = "passage: ",
        input_names = InputNames::bert(), output_name = default_output_name(), pooling = PoolingStrategy::Mean,
        // EX-V1.29-6: same base architecture as e5-base; ONNX bundle ~440 MiB.
        approx_download_bytes = Some(440 * 1024 * 1024),
        pad_id = 0;

    /// BGE-large-en-v1.5: 1024-dim, 512 tokens. Strong general-purpose
    /// retriever; was the cqs default through v1.34.x. Replaced as default
    /// by `embeddinggemma-300m` in v1.35.0 (R@1 +1.9pp on v3.v2 dual-judge,
    /// half the params, 4× context window).
    ///
    /// Standard BERT I/O, mean pooling (matches the BGE-reference implementation
    /// used in HuggingFace `sentence-transformers`).
    bge_large => name = "bge-large", repo = "BAAI/bge-large-en-v1.5",
        onnx_path = "onnx/model.onnx", tokenizer_path = "tokenizer.json",
        dim = 1024, max_seq_length = 512,
        query_prefix = "Represent this sentence for searching relevant passages: ", doc_prefix = "",
        input_names = InputNames::bert(), output_name = default_output_name(), pooling = PoolingStrategy::Mean,
        // EX-V1.29-6: full BGE-large ONNX bundle ~1.3 GiB.
        approx_download_bytes = Some(1_300 * 1024 * 1024),
        pad_id = 0;

    /// BGE-large-ft: LoRA fine-tune of BGE-large-en-v1.5 on cqs's
    /// `cqs-code-search-200k` dataset (`jamie8johnson/cqs-code-search-200k`).
    /// Same architecture, dim, max_seq, and prefixes as the upstream
    /// BGE-large preset — adapters merged into the ONNX export.
    ///
    /// Best R@5 on v3.v2 dual-judge (73.4% agg vs default 72.5%);
    /// recommended for R@5-sensitive flows where broader top-K is needed
    /// before reranking. Top-1 trails the default by 1.4pp.
    ///
    /// Set `CQS_EMBEDDING_MODEL=bge-large-ft` or
    /// `cqs slot create bge-ft --model bge-large-ft` to use it.
    bge_large_ft => name = "bge-large-ft", repo = "jamie8johnson/bge-large-v1.5-code-search",
        onnx_path = "onnx/model.onnx", tokenizer_path = "onnx/tokenizer.json",
        dim = 1024, max_seq_length = 512,
        query_prefix = "Represent this sentence for searching relevant passages: ", doc_prefix = "",
        input_names = InputNames::bert(), output_name = default_output_name(), pooling = PoolingStrategy::Mean,
        // Same architecture as base BGE-large, ~1.3 GiB ONNX bundle.
        approx_download_bytes = Some(1_300 * 1024 * 1024),
        pad_id = 0;

    /// CodeRankEmbed: 768-dim, 2048 tokens. Code-specialized fine-tune of
    /// Snowflake Arctic Embed M Long, trained on CoRNStack (~21M code pairs).
    /// Headline: 77.9 MRR on CodeSearchNet, 60.1 NDCG@10 on CoIR.
    ///
    /// Architecture is NomicBERT (custom: SwiGLU activation, RoPE, fused ops);
    /// the ONNX export packages those into standard ONNX ops. Two inputs only
    /// (`input_ids` + `attention_mask`, no `token_type_ids`); CLS pooling per
    /// the model's `1_Pooling/config.json`. The official repo ships
    /// safetensors only — this preset points at a re-exported ONNX bundle.
    nomic_coderank => name = "nomic-coderank", repo = "jamie8johnson/CodeRankEmbed-onnx",
        onnx_path = "onnx/model.onnx", tokenizer_path = "tokenizer.json",
        dim = 768, max_seq_length = 2048,
        query_prefix = "Represent this query for searching relevant code: ", doc_prefix = "",
        // The sentence-transformers ONNX export exposes `token_embeddings`
        // (3D `[batch, seq, dim]`) plus a pre-pooled `sentence_embedding`
        // (2D). cqs's pooling expects a 3D tensor, so we read
        // `token_embeddings` and CLS-pool ourselves.
        input_names = InputNames::bert_no_token_types(), output_name = "token_embeddings".to_string(), pooling = PoolingStrategy::Cls,
        // ONNX bundle: 522 MiB model + 712 KiB tokenizer.
        approx_download_bytes = Some(523 * 1024 * 1024),
        pad_id = 0;

    /// EmbeddingGemma-300m: 768-dim, 2048 tokens. Google's Gemma3-backbone
    /// embedder (Sep 2025), 308M params. #1 multilingual under 500M on MTEB
    /// at release. **2K context** (vs BERT-family 512), MRL-truncatable to
    /// 768/512/256/128, task-instruction prompt format.
    ///
    /// Architecture: Gemma3 decoder + bidirectional-attention head. Two ONNX
    /// inputs (`input_ids` + `attention_mask`, no `token_type_ids`). The
    /// FP16 ONNX export from `onnx-community/embeddinggemma-300m-ONNX` ships
    /// both `last_hidden_state` (3D `[batch, seq, 768]`) and
    /// `sentence_embedding` (2D `[batch, 768]`) — but the latter is computed
    /// by a task-aware projection head that cannot be reproduced by
    /// mean/cls/last over `last_hidden_state` (verified: cosine ≈ 0 between
    /// any naive pool and the model's own pooled output). Use
    /// `PoolingStrategy::Identity` to consume the pre-pooled output
    /// directly.
    ///
    /// Tokenizer: Gemma3 SentencePiece, 256k vocab, `<bos>`-prefixed +
    /// `<eos>`-suffixed via the post-processor in `tokenizer.json`. The
    /// tokenizer file is ~20 MB (vs BERT's ~700 KB) due to the larger vocab.
    ///
    /// Query/doc prefix follows Google's recommended task-instruction
    /// format: `task: search result | query: …` for queries,
    /// `title: none | text: …` for documents. cqs's existing
    /// `query_prefix` / `doc_prefix` plumbing handles this directly — the
    /// prefix is just a longer string than E5/BGE use.
    embeddinggemma_300m => name = "embeddinggemma-300m", repo = "onnx-community/embeddinggemma-300m-ONNX",
        onnx_path = "onnx/model.onnx", tokenizer_path = "tokenizer.json",
        dim = 768, max_seq_length = 2048,
        query_prefix = "task: search result | query: ", doc_prefix = "title: none | text: ",
        input_names = InputNames::bert_no_token_types(),
        output_name = "sentence_embedding".to_string(),
        pooling = PoolingStrategy::Identity,
        // FP32 ONNX bundle: ~1.2 GB model + ~20 MB tokenizer + small configs.
        // FP16 (`onnx/model_fp16.onnx`, ~617 MB) is also published but
        // produces NaN/Inf in attention layers under CUDA EP — the
        // mixed-precision auto-fallback that TensorRT/TRT-RTX would
        // provide isn't there. Stay on FP32 until we wire TRT-RTX or
        // confirm a different EP handles FP16 cleanly.
        approx_download_bytes = Some(1_300 * 1024 * 1024),
        pad_id = 0,
        default = true;
}

impl ModelConfig {
    /// Resolve model config for a **query path** against an existing index.
    ///
    /// When the index records a model name (`Store::stored_model_name()`), that
    /// name wins over CLI flag / env / config / default. The reasoning:
    /// the index was built with a specific embedder, and the only useful
    /// embedder for querying it is the one whose dim matches. Honouring a CLI
    /// flag or `CQS_EMBEDDING_MODEL` that points at a different model leads to
    /// the silent "0 results, only a tracing::warn!" failure mode this method
    /// was added to prevent (see ROADMAP.md "Embedder swap workflow").
    ///
    /// Index time (`cqs index --force`) must keep using [`resolve`] — there
    /// the user's intent is precisely to install a new embedder, and the
    /// stored name (if any) is about to be overwritten.
    ///
    /// Falls through to [`resolve`] when:
    /// - `stored_model` is `None` (fresh project, or pre-model-name index)
    /// - the stored name is not a known preset (custom config — caller's
    ///   chain still matches)
    pub fn resolve_for_query(
        stored_model: Option<&str>,
        cli_model: Option<&str>,
        config_embedding: Option<&EmbeddingConfig>,
    ) -> Self {
        let _span = tracing::info_span!("resolve_model_config_for_query").entered();
        if let Some(name) = stored_model {
            if let Some(cfg) = Self::from_preset(name) {
                tracing::info!(
                    model = %cfg.name,
                    source = "index",
                    "Resolved model config from indexed model name"
                );
                return cfg;
            }
            // Stored name not a known preset — fall through. The caller's
            // resolution chain (CLI / env / config / default) is the next
            // best signal. The defensive `Store::check_query_dim` guard will
            // still catch a dim mismatch with an actionable error.
            tracing::debug!(
                stored = %name,
                "Stored model name is not a known preset, falling back to CLI/env/config resolution"
            );
        }
        Self::resolve(cli_model, config_embedding)
    }

    /// Resolve model config from (in priority order): CLI flag, env var, config file, default.
    ///
    /// Unknown preset names log a warning and fall back to default.
    pub fn resolve(cli_model: Option<&str>, config_embedding: Option<&EmbeddingConfig>) -> Self {
        let _span = tracing::info_span!("resolve_model_config").entered();

        // 1. CLI flag (highest priority)
        if let Some(name) = cli_model {
            if let Some(cfg) = Self::from_preset(name) {
                tracing::info!(model = %cfg.name, source = "cli", "Resolved model config");
                return cfg;
            }
            tracing::warn!(
                model = name,
                "Unknown model from CLI flag, falling back to default"
            );
            return Self::default_model();
        }

        // 2. Environment variable
        if let Ok(env_val) = std::env::var("CQS_EMBEDDING_MODEL") {
            if !env_val.is_empty() {
                if let Some(cfg) = Self::from_preset(&env_val) {
                    tracing::info!(model = %cfg.name, source = "env", "Resolved model config");
                    return cfg;
                }
                tracing::warn!(
                    model = %env_val,
                    "Unknown CQS_EMBEDDING_MODEL env var value, falling back to default"
                );
                return Self::default_model();
            }
        }

        // 3. Config file
        if let Some(embedding_cfg) = config_embedding {
            if let Some(cfg) = Self::from_preset(&embedding_cfg.model) {
                tracing::info!(model = %cfg.name, source = "config", "Resolved model config");
                return cfg;
            }
            // Not a known preset — check if custom fields are present
            let has_repo = embedding_cfg.repo.is_some();
            let has_dim = embedding_cfg.dim.is_some();
            if has_repo && has_dim {
                let dim = embedding_cfg.dim.expect("guarded by has_dim");
                if dim == 0 {
                    tracing::warn!(model = %embedding_cfg.model, "Custom model has dim=0, falling back to default");
                    return Self::default_model();
                }

                // SEC-28: Validate repo format — must be "org/model" without injection chars
                let repo = embedding_cfg.repo.as_ref().expect("guarded by has_repo");
                if !repo.contains('/')
                    || repo.contains('"')
                    || repo.contains('\n')
                    || repo.contains('\\')
                    || repo.contains(' ')
                    || repo.starts_with('/')
                    || repo.contains("..")
                {
                    tracing::warn!(
                        %repo,
                        "Custom model repo contains invalid characters, falling back to default"
                    );
                    return Self::default_model();
                }

                // SEC-20: Validate custom paths don't contain traversal
                let onnx_path = embedding_cfg
                    .onnx_path
                    .clone()
                    .unwrap_or_else(|| "onnx/model.onnx".to_string());
                let tokenizer_path = embedding_cfg
                    .tokenizer_path
                    .clone()
                    .unwrap_or_else(|| "tokenizer.json".to_string());
                for (label, path) in [
                    ("onnx_path", &onnx_path),
                    ("tokenizer_path", &tokenizer_path),
                ] {
                    if path.contains("..") || std::path::Path::new(path).is_absolute() {
                        tracing::warn!(%label, %path, "Custom model path contains traversal or is absolute, falling back to default");
                        return Self::default_model();
                    }
                }

                // Architecture fields: fall back to BERT defaults if the user
                // did not override them. The tokenizer auto-detects BPE vs
                // WordPiece from `tokenizer.json`, so no tokenizer_kind needed.
                let input_names = embedding_cfg
                    .input_names
                    .clone()
                    .unwrap_or_else(InputNames::bert);
                let output_name = embedding_cfg
                    .output_name
                    .clone()
                    .unwrap_or_else(default_output_name);
                let pooling = embedding_cfg.pooling.unwrap_or(PoolingStrategy::Mean);

                let cfg = Self {
                    name: embedding_cfg.model.clone(),
                    repo: embedding_cfg.repo.clone().expect("guarded by has_repo"),
                    onnx_path,
                    tokenizer_path,
                    dim,
                    max_seq_length: embedding_cfg.max_seq_length.unwrap_or(512),
                    query_prefix: embedding_cfg.query_prefix.clone().unwrap_or_default(),
                    doc_prefix: embedding_cfg.doc_prefix.clone().unwrap_or_default(),
                    input_names,
                    output_name,
                    pooling,
                    approx_download_bytes: None,
                    // SHL-V1.29-1: custom model — fall back to `0` and let
                    // the encoder's session-init probe the tokenizer for a
                    // declared pad id. `0` matches every BERT-family model.
                    pad_id: embedding_cfg.pad_id.unwrap_or(0),
                };
                tracing::info!(model = %cfg.name, source = "config-custom", "Resolved custom model config");
                return cfg;
            }
            tracing::warn!(
                model = %embedding_cfg.model,
                has_repo,
                has_dim,
                "Unknown model in config and missing required custom fields (repo, dim), falling back to default"
            );
        }

        // 4. Default — see `define_embedder_presets!` for the row marked
        // `default = true`. EmbeddingGemma-300m since v1.35.0; BGE-large
        // before that.
        let dm = Self::default_model();
        tracing::info!(
            model = %dm.name,
            source = "default",
            "Resolved model config"
        );
        dm
    }

    /// SHL-V1.30-1 / P2.41 — scale the embed batch size with this model's
    /// dim & seq, holding the per-tensor footprint roughly constant.
    ///
    /// BGE-large (1024 dim, 512 seq) at batch=64 ≈ 130 MB per forward-pass
    /// tensor — the empirical sweet spot on RTX 4060 8 GB. Nomic-coderank
    /// (768 dim, 2048 seq) at batch=64 OOMs the same GPU because the tensor
    /// blows up to ~390 MB.
    ///
    /// Formula: `batch_baseline * (1024/dim) * (512/seq)` rounded to a
    /// power of 2, clamped to `[2, 256]`. The env override
    /// `CQS_EMBED_BATCH_SIZE` takes priority — operators with workloads
    /// they understand can pin a value.
    pub fn embed_batch_size(&self) -> usize {
        if let Ok(val) = std::env::var("CQS_EMBED_BATCH_SIZE") {
            if let Ok(size) = val.parse::<usize>() {
                if size > 0 {
                    tracing::info!(batch_size = size, "CQS_EMBED_BATCH_SIZE override");
                    return size;
                }
            }
            tracing::warn!(
                value = %val,
                "Invalid CQS_EMBED_BATCH_SIZE, falling back to model-derived default"
            );
        }
        let dim = self.dim.max(1) as f64;
        let seq = self.max_seq_length.max(1) as f64;
        let baseline = 64.0_f64;
        let dim_factor = 1024.0 / dim;
        let seq_factor = (512.0 / seq).max(0.25);
        let scaled = (baseline * dim_factor * seq_factor).max(1.0) as usize;
        let rounded = scaled.next_power_of_two().clamp(2, 256);
        tracing::debug!(
            dim = self.dim,
            seq = self.max_seq_length,
            scaled,
            rounded,
            "ModelConfig::embed_batch_size: model-derived default"
        );
        rounded
    }

    /// Apply env var overrides to a resolved ModelConfig.
    /// CQS_MAX_SEQ_LENGTH overrides max_seq_length (for large-context models via CQS_ONNX_DIR).
    /// CQS_EMBEDDING_DIM overrides dim (for custom models where dim detection isn't automatic).
    pub fn apply_env_overrides(mut self) -> Self {
        if let Ok(val) = std::env::var("CQS_MAX_SEQ_LENGTH") {
            if let Ok(seq) = val.parse::<usize>() {
                tracing::info!(max_seq_length = seq, "CQS_MAX_SEQ_LENGTH override active");
                self.max_seq_length = seq;
            }
        }
        if let Ok(val) = std::env::var("CQS_EMBEDDING_DIM") {
            if let Ok(dim) = val.parse::<usize>() {
                if dim > 0 {
                    tracing::info!(dim, "CQS_EMBEDDING_DIM override active");
                    self.dim = dim;
                }
            }
        }
        self
    }
}

/// Config-file section for embedding model settings.
///
/// Parsed from `[embedding]` in the cqs config file.
/// All fields except `model` are optional — preset names fill them automatically,
/// and architecture fields (`input_names`, `output_name`, `pooling`) fall back
/// to BERT defaults when absent.
#[derive(Debug, Clone, Deserialize)]
pub struct EmbeddingConfig {
    /// Model name or preset (defaults to the row marked `default = true` in
    /// `define_embedder_presets!` — `embeddinggemma-300m` since v1.35.0).
    #[serde(default = "default_model_name")]
    pub model: String,
    /// HuggingFace repo ID (required for custom models)
    pub repo: Option<String>,
    /// ONNX model path within repo
    pub onnx_path: Option<String>,
    /// Tokenizer path within repo
    pub tokenizer_path: Option<String>,
    /// Embedding dimension (required for custom models)
    pub dim: Option<usize>,
    /// Max sequence length
    pub max_seq_length: Option<usize>,
    /// Query prefix
    pub query_prefix: Option<String>,
    /// Document prefix
    pub doc_prefix: Option<String>,
    /// Names of the ONNX input tensors (defaults to BERT: `input_ids`,
    /// `attention_mask`, `token_type_ids`). Omit for BERT-family models.
    #[serde(default)]
    pub input_names: Option<InputNames>,
    /// Output tensor to pool over (default `last_hidden_state`).
    #[serde(default)]
    pub output_name: Option<String>,
    /// Pooling strategy (`mean`, `cls`, or `lasttoken`; default `mean`).
    #[serde(default)]
    pub pooling: Option<PoolingStrategy>,
    /// SHL-V1.29-1: Pad token id for `input_ids` padding (default `0`).
    ///
    /// Override only when the custom tokenizer declares a non-zero pad id
    /// and `tokenizer.json` doesn't carry a usable `padding` section for
    /// the encoder to read at session init.
    #[serde(default)]
    pub pad_id: Option<i64>,
}

fn default_model_name() -> String {
    ModelConfig::default_model().name
}

impl Default for EmbeddingConfig {
    /// All-`None` defaults with `model` set to the project default.
    ///
    /// Intended as a starting point for tests / programmatic config — the
    /// `resolve()` path fills in architecture fields (input_names, output_name,
    /// pooling) when the user does not override them.
    fn default() -> Self {
        Self {
            model: default_model_name(),
            repo: None,
            onnx_path: None,
            tokenizer_path: None,
            dim: None,
            max_seq_length: None,
            query_prefix: None,
            doc_prefix: None,
            input_names: None,
            output_name: None,
            pooling: None,
            pad_id: None,
        }
    }
}

/// Model metadata for index initialization.
///
/// Construct via `ModelInfo::new()` with explicit name + dim, or
/// `ModelInfo::default()` for tests only (project default model, currently
/// EmbeddingGemma-300m, 768-dim).
#[derive(Debug, Clone, serde::Serialize)]
pub struct ModelInfo {
    pub name: String,
    pub dimensions: usize,
    pub version: String,
}

impl ModelInfo {
    /// Create ModelInfo with explicit model name and dimension.
    ///
    /// This is the preferred constructor for production code. The name and dim
    /// come from the Embedder at runtime.
    pub fn new(name: impl Into<String>, dim: usize) -> Self {
        ModelInfo {
            name: name.into(),
            dimensions: dim,
            version: "2".to_string(),
        }
    }

    /// Create ModelInfo with default model name and a specific dimension.
    ///
    /// Convenience for callers that only vary dimension (e.g., `Embedder::embedding_dim()`).
    pub fn with_dim(dim: usize) -> Self {
        Self::new(ModelConfig::default_model().repo, dim)
    }
}

impl Default for ModelInfo {
    /// Test-only default: project default model (currently EmbeddingGemma-300m, 768-dim).
    ///
    /// Production code should use `ModelInfo::new()` or `ModelInfo::with_dim()`.
    /// All fields derive from `ModelConfig::default_model()` — change the
    /// `default = true` marker in `define_embedder_presets!` to switch.
    fn default() -> Self {
        let cfg = ModelConfig::default_model();
        ModelInfo {
            name: cfg.repo,
            dimensions: cfg.dim,
            version: "2".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Mutex to serialize tests that manipulate CQS_EMBEDDING_MODEL env var.
    /// Env vars are process-global — concurrent test threads race on set/remove.
    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    // ===== Preset tests =====

    #[test]
    fn test_e5_base_preset() {
        let cfg = ModelConfig::e5_base();
        assert_eq!(cfg.name, "e5-base");
        assert_eq!(cfg.repo, "intfloat/e5-base-v2");
        assert_eq!(cfg.dim, 768);
        assert_eq!(cfg.max_seq_length, 512);
        assert_eq!(cfg.query_prefix, "query: ");
        assert_eq!(cfg.doc_prefix, "passage: ");
        assert_eq!(cfg.onnx_path, "onnx/model.onnx");
        assert_eq!(cfg.tokenizer_path, "tokenizer.json");
        // Architecture: BERT inputs + last_hidden_state + mean pooling
        assert_eq!(cfg.input_names, InputNames::bert());
        assert_eq!(cfg.output_name, "last_hidden_state");
        assert_eq!(cfg.pooling, PoolingStrategy::Mean);
    }

    #[test]
    fn test_bge_large_preset() {
        let cfg = ModelConfig::bge_large();
        assert_eq!(cfg.name, "bge-large");
        assert_eq!(cfg.repo, "BAAI/bge-large-en-v1.5");
        assert_eq!(cfg.dim, 1024);
        assert_eq!(cfg.max_seq_length, 512);
        assert_eq!(
            cfg.query_prefix,
            "Represent this sentence for searching relevant passages: "
        );
        assert_eq!(cfg.doc_prefix, "");
        // Architecture: BERT inputs + last_hidden_state + mean pooling
        assert_eq!(cfg.input_names, InputNames::bert());
        assert_eq!(cfg.output_name, "last_hidden_state");
        assert_eq!(cfg.pooling, PoolingStrategy::Mean);
    }

    #[test]
    fn test_v9_200k_preset() {
        let cfg = ModelConfig::v9_200k();
        assert_eq!(cfg.name, "v9-200k");
        assert_eq!(cfg.repo, "jamie8johnson/e5-base-v2-code-search");
        assert_eq!(cfg.dim, 768);
        assert_eq!(cfg.onnx_path, "model.onnx");
        assert_eq!(cfg.query_prefix, "query: ");
        assert_eq!(cfg.doc_prefix, "passage: ");
        // Architecture: BERT inputs + last_hidden_state + mean pooling
        assert_eq!(cfg.input_names, InputNames::bert());
        assert_eq!(cfg.output_name, "last_hidden_state");
        assert_eq!(cfg.pooling, PoolingStrategy::Mean);
    }

    // ===== Architecture type tests =====

    #[test]
    fn input_names_bert_defaults() {
        let n = InputNames::bert();
        assert_eq!(n.ids, "input_ids");
        assert_eq!(n.mask, "attention_mask");
        assert_eq!(n.token_types.as_deref(), Some("token_type_ids"));
    }

    #[test]
    fn input_names_no_token_types() {
        let n = InputNames::bert_no_token_types();
        assert_eq!(n.ids, "input_ids");
        assert_eq!(n.mask, "attention_mask");
        assert!(
            n.token_types.is_none(),
            "bert_no_token_types should drop segment embeddings"
        );
    }

    #[test]
    fn input_names_default_matches_bert() {
        assert_eq!(InputNames::default(), InputNames::bert());
    }

    #[test]
    fn input_names_serde_empty_fills_defaults() {
        // `{}` should fill in both string fields via serde defaults,
        // leaving token_types = None.
        let parsed: InputNames = serde_json::from_str("{}").unwrap();
        assert_eq!(parsed.ids, "input_ids");
        assert_eq!(parsed.mask, "attention_mask");
        assert!(parsed.token_types.is_none());
    }

    #[test]
    fn input_names_serde_custom() {
        let j = r#"{ "ids": "tokens", "mask": "mask", "token_types": null }"#;
        let parsed: InputNames = serde_json::from_str(j).unwrap();
        assert_eq!(parsed.ids, "tokens");
        assert_eq!(parsed.mask, "mask");
        assert!(parsed.token_types.is_none());
    }

    #[test]
    fn pooling_strategy_serde_roundtrip() {
        // The serde rename_all = "lowercase" rule means we accept
        // "mean" / "cls" / "lasttoken".
        let mean: PoolingStrategy = serde_json::from_str("\"mean\"").unwrap();
        assert_eq!(mean, PoolingStrategy::Mean);
        let cls: PoolingStrategy = serde_json::from_str("\"cls\"").unwrap();
        assert_eq!(cls, PoolingStrategy::Cls);
        let last: PoolingStrategy = serde_json::from_str("\"lasttoken\"").unwrap();
        assert_eq!(last, PoolingStrategy::LastToken);
    }

    #[test]
    fn pooling_strategy_default_is_mean() {
        assert_eq!(PoolingStrategy::default(), PoolingStrategy::Mean);
    }

    // Synthetic non-BERT preset test: prove that a custom EmbeddingConfig
    // declaring CLS pooling + no token_types flows through resolve() without
    // losing those overrides. This is the plumbing test — actual encoding
    // against a real non-BERT model is out of scope for unit tests.
    #[test]
    fn resolve_custom_non_bert_architecture() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::remove_var("CQS_EMBEDDING_MODEL");
        let embedding_cfg = EmbeddingConfig {
            model: "synthetic-distilbert".to_string(),
            repo: Some("org/distil".to_string()),
            onnx_path: Some("model.onnx".to_string()),
            tokenizer_path: None,
            dim: Some(384),
            max_seq_length: Some(128),
            query_prefix: None,
            doc_prefix: None,
            input_names: Some(InputNames::bert_no_token_types()),
            output_name: Some("sentence_embedding".to_string()),
            pooling: Some(PoolingStrategy::Cls),
            pad_id: None,
        };
        let resolved = ModelConfig::resolve(None, Some(&embedding_cfg));
        assert_eq!(resolved.name, "synthetic-distilbert");
        assert_eq!(resolved.dim, 384);
        assert_eq!(resolved.pooling, PoolingStrategy::Cls);
        assert_eq!(resolved.output_name, "sentence_embedding");
        assert!(
            resolved.input_names.token_types.is_none(),
            "Custom config must not re-introduce token_type_ids"
        );
    }

    // If architecture fields are absent from a custom config, resolve() must
    // default to BERT + last_hidden_state + mean — i.e. existing custom configs
    // (pre-949) keep working unchanged.
    #[test]
    fn resolve_custom_without_architecture_uses_bert_defaults() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::remove_var("CQS_EMBEDDING_MODEL");
        let mut cfg = EmbeddingConfig::default();
        cfg.model = "legacy-custom".to_string();
        cfg.repo = Some("org/legacy".to_string());
        cfg.dim = Some(768);
        // No input_names / output_name / pooling overrides.
        let resolved = ModelConfig::resolve(None, Some(&cfg));
        assert_eq!(resolved.name, "legacy-custom");
        assert_eq!(resolved.input_names, InputNames::bert());
        assert_eq!(resolved.output_name, "last_hidden_state");
        assert_eq!(resolved.pooling, PoolingStrategy::Mean);
    }

    #[test]
    fn embedding_config_serde_with_architecture() {
        // Full roundtrip including pooling + input_names from JSON.
        let json = r#"{
            "model": "custom",
            "repo": "org/model",
            "dim": 768,
            "pooling": "cls",
            "output_name": "pooled",
            "input_names": { "ids": "tok", "mask": "m" }
        }"#;
        let cfg: EmbeddingConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.pooling, Some(PoolingStrategy::Cls));
        assert_eq!(cfg.output_name.as_deref(), Some("pooled"));
        let names = cfg.input_names.as_ref().unwrap();
        assert_eq!(names.ids, "tok");
        assert_eq!(names.mask, "m");
        assert!(
            names.token_types.is_none(),
            "Absent token_types deserializes to None"
        );
    }

    #[test]
    fn embedding_config_serde_without_architecture_keeps_all_none() {
        // Absent fields mean "use BERT defaults later in resolve()".
        let json = r#"{ "model": "bge-large" }"#;
        let cfg: EmbeddingConfig = serde_json::from_str(json).unwrap();
        assert!(cfg.pooling.is_none());
        assert!(cfg.output_name.is_none());
        assert!(cfg.input_names.is_none());
    }

    // ===== from_preset tests =====

    #[test]
    fn test_from_preset_short_name() {
        assert!(ModelConfig::from_preset("e5-base").is_some());
        assert!(ModelConfig::from_preset("v9-200k").is_some());
        assert!(ModelConfig::from_preset("bge-large").is_some());
    }

    #[test]
    fn test_from_preset_repo_id() {
        let cfg = ModelConfig::from_preset("intfloat/e5-base-v2").unwrap();
        assert_eq!(cfg.name, "e5-base");

        let cfg = ModelConfig::from_preset("jamie8johnson/e5-base-v2-code-search").unwrap();
        assert_eq!(cfg.name, "v9-200k");

        let cfg = ModelConfig::from_preset("BAAI/bge-large-en-v1.5").unwrap();
        assert_eq!(cfg.name, "bge-large");
    }

    #[test]
    fn test_from_preset_unknown() {
        assert!(ModelConfig::from_preset("unknown-model").is_none());
        assert!(ModelConfig::from_preset("").is_none());
    }

    // ===== resolve tests =====

    #[test]
    fn test_resolve_default() {
        let _lock = ENV_MUTEX.lock().unwrap();
        // Clear env to ensure we get default
        std::env::remove_var("CQS_EMBEDDING_MODEL");
        let cfg = ModelConfig::resolve(None, None);
        assert_eq!(cfg.name, ModelConfig::default_model().name);
    }

    #[test]
    fn test_resolve_env_by_name() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::set_var("CQS_EMBEDDING_MODEL", "bge-large");
        let cfg = ModelConfig::resolve(None, None);
        assert_eq!(cfg.name, "bge-large");
        std::env::remove_var("CQS_EMBEDDING_MODEL");
    }

    #[test]
    fn test_resolve_env_by_repo_id() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::set_var("CQS_EMBEDDING_MODEL", "BAAI/bge-large-en-v1.5");
        let cfg = ModelConfig::resolve(None, None);
        assert_eq!(cfg.name, "bge-large");
        std::env::remove_var("CQS_EMBEDDING_MODEL");
    }

    #[test]
    fn test_resolve_cli_overrides_env() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::set_var("CQS_EMBEDDING_MODEL", "bge-large");
        let cfg = ModelConfig::resolve(Some("e5-base"), None);
        assert_eq!(cfg.name, "e5-base");
        std::env::remove_var("CQS_EMBEDDING_MODEL");
    }

    #[test]
    fn test_resolve_unknown_env_warns_and_defaults() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::set_var("CQS_EMBEDDING_MODEL", "nonexistent-model");
        let cfg = ModelConfig::resolve(None, None);
        assert_eq!(cfg.name, ModelConfig::default_model().name); // falls back to default
        std::env::remove_var("CQS_EMBEDDING_MODEL");
    }

    #[test]
    fn test_resolve_unknown_cli_warns_and_defaults() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let cfg = ModelConfig::resolve(Some("nonexistent"), None);
        assert_eq!(cfg.name, ModelConfig::default_model().name);
    }

    #[test]
    fn test_resolve_config_preset() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::remove_var("CQS_EMBEDDING_MODEL");
        let embedding_cfg = EmbeddingConfig {
            model: "bge-large".to_string(),
            repo: None,
            onnx_path: None,
            tokenizer_path: None,
            dim: None,
            max_seq_length: None,
            query_prefix: None,
            doc_prefix: None,
            input_names: None,
            output_name: None,
            pooling: None,
            pad_id: None,
        };
        let cfg = ModelConfig::resolve(None, Some(&embedding_cfg));
        assert_eq!(cfg.name, "bge-large");
    }

    #[test]
    fn test_resolve_config_custom_model() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::remove_var("CQS_EMBEDDING_MODEL");
        let embedding_cfg = EmbeddingConfig {
            model: "my-custom".to_string(),
            repo: Some("my-org/my-model".to_string()),
            onnx_path: Some("model.onnx".to_string()),
            tokenizer_path: None,
            dim: Some(384),
            max_seq_length: Some(256),
            query_prefix: Some("search: ".to_string()),
            doc_prefix: None,
            input_names: None,
            output_name: None,
            pooling: None,
            pad_id: None,
        };
        let cfg = ModelConfig::resolve(None, Some(&embedding_cfg));
        assert_eq!(cfg.name, "my-custom");
        assert_eq!(cfg.repo, "my-org/my-model");
        assert_eq!(cfg.dim, 384);
        assert_eq!(cfg.max_seq_length, 256);
        assert_eq!(cfg.onnx_path, "model.onnx");
        assert_eq!(cfg.tokenizer_path, "tokenizer.json"); // default
        assert_eq!(cfg.query_prefix, "search: ");
        assert_eq!(cfg.doc_prefix, ""); // default
    }

    #[test]
    fn test_resolve_config_unknown_missing_fields_defaults() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::remove_var("CQS_EMBEDDING_MODEL");
        let embedding_cfg = EmbeddingConfig {
            model: "unknown".to_string(),
            repo: None, // missing required field
            onnx_path: None,
            tokenizer_path: None,
            dim: None, // missing required field
            max_seq_length: None,
            query_prefix: None,
            doc_prefix: None,
            input_names: None,
            output_name: None,
            pooling: None,
            pad_id: None,
        };
        let cfg = ModelConfig::resolve(None, Some(&embedding_cfg));
        assert_eq!(cfg.name, ModelConfig::default_model().name); // falls back
    }

    // ===== EmbeddingConfig serde tests =====

    #[test]
    fn test_embedding_config_default_model() {
        let json = r#"{}"#;
        let cfg: EmbeddingConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.model, ModelConfig::default_model().name);
    }

    #[test]
    fn test_embedding_config_explicit_model() {
        let json = r#"{"model": "bge-large"}"#;
        let cfg: EmbeddingConfig = serde_json::from_str(json).unwrap();
        // Explicit model name overrides default — bge-large stays valid as a preset.
        assert_eq!(cfg.model, "bge-large");
    }

    #[test]
    fn test_embedding_config_custom_fields() {
        let json = r#"{
            "model": "custom",
            "repo": "org/model",
            "dim": 384,
            "query_prefix": "q: "
        }"#;
        let cfg: EmbeddingConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.model, "custom");
        assert_eq!(cfg.repo.unwrap(), "org/model");
        assert_eq!(cfg.dim.unwrap(), 384);
        assert_eq!(cfg.query_prefix.unwrap(), "q: ");
        assert!(cfg.doc_prefix.is_none());
    }

    #[test]
    fn test_resolve_empty_env_ignored() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::set_var("CQS_EMBEDDING_MODEL", "");
        let cfg = ModelConfig::resolve(None, None);
        assert_eq!(cfg.name, ModelConfig::default_model().name);
        std::env::remove_var("CQS_EMBEDDING_MODEL");
    }

    #[test]
    fn test_resolve_cli_overrides_config() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::remove_var("CQS_EMBEDDING_MODEL");
        let embedding_cfg = EmbeddingConfig {
            model: "bge-large".to_string(),
            repo: None,
            onnx_path: None,
            tokenizer_path: None,
            dim: None,
            max_seq_length: None,
            query_prefix: None,
            doc_prefix: None,
            input_names: None,
            output_name: None,
            pooling: None,
            pad_id: None,
        };
        let cfg = ModelConfig::resolve(Some("e5-base"), Some(&embedding_cfg));
        assert_eq!(cfg.name, "e5-base");
    }

    // ===== CQ-V1.33.0-2: embed_batch_size scales with model dim/seq =====

    /// BGE-large (1024 dim, 512 seq) — the calibration baseline. Scales to 64.
    #[test]
    fn embed_batch_size_bge_large_baseline() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::remove_var("CQS_EMBED_BATCH_SIZE");
        assert_eq!(ModelConfig::bge_large().embed_batch_size(), 64);
    }

    /// E5-base (768 dim, 512 seq) — same seq, smaller dim → batch up to 128.
    #[test]
    fn embed_batch_size_e5_base_scales_up() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::remove_var("CQS_EMBED_BATCH_SIZE");
        let cfg = ModelConfig::e5_base();
        assert_eq!(cfg.dim, 768);
        assert_eq!(cfg.max_seq_length, 512);
        // 64 * (1024/768) * (512/512) ≈ 85 → next_power_of_two = 128
        assert_eq!(cfg.embed_batch_size(), 128);
    }

    /// Synthetic nomic-coderank shape (768 dim, 2048 seq). The OOM-on-8GB
    /// failure mode this method exists to fix — batch must drop to <= 32.
    #[test]
    fn embed_batch_size_nomic_coderank_shape_drops_on_long_seq() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::remove_var("CQS_EMBED_BATCH_SIZE");
        let cfg = ModelConfig {
            name: "nomic-coderank-test".to_string(),
            repo: "test/test".to_string(),
            onnx_path: "model.onnx".to_string(),
            tokenizer_path: "tokenizer.json".to_string(),
            dim: 768,
            max_seq_length: 2048,
            query_prefix: String::new(),
            doc_prefix: String::new(),
            input_names: InputNames::bert(),
            output_name: "last_hidden_state".to_string(),
            pooling: PoolingStrategy::Mean,
            approx_download_bytes: None,
            pad_id: 0,
        };
        // 64 * (1024/768) * (512/2048) ≈ 21 → next_power_of_two = 32
        let bs = cfg.embed_batch_size();
        assert!(
            bs <= 32,
            "nomic-coderank shape must drop batch to <= 32, got {}",
            bs
        );
    }

    /// Env override wins regardless of model shape.
    #[test]
    fn embed_batch_size_env_override_wins() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::set_var("CQS_EMBED_BATCH_SIZE", "16");
        assert_eq!(ModelConfig::bge_large().embed_batch_size(), 16);
        std::env::remove_var("CQS_EMBED_BATCH_SIZE");
    }

    /// Invalid env override falls through to model-derived default.
    #[test]
    fn embed_batch_size_invalid_env_falls_through() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::set_var("CQS_EMBED_BATCH_SIZE", "not_a_number");
        assert_eq!(ModelConfig::bge_large().embed_batch_size(), 64);
        std::env::remove_var("CQS_EMBED_BATCH_SIZE");
    }

    /// Zero env override falls through (defends against div-by-zero).
    #[test]
    fn embed_batch_size_zero_env_falls_through() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::set_var("CQS_EMBED_BATCH_SIZE", "0");
        assert_eq!(ModelConfig::bge_large().embed_batch_size(), 64);
        std::env::remove_var("CQS_EMBED_BATCH_SIZE");
    }

    /// Output is always clamped to [2, 256].
    #[test]
    fn embed_batch_size_clamped() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::remove_var("CQS_EMBED_BATCH_SIZE");
        // Synthetic large-dim/long-seq → would fall to <2 unclamped.
        let extreme = ModelConfig {
            name: "extreme".to_string(),
            repo: "test/test".to_string(),
            onnx_path: "model.onnx".to_string(),
            tokenizer_path: "tokenizer.json".to_string(),
            dim: 65536,
            max_seq_length: 65536,
            query_prefix: String::new(),
            doc_prefix: String::new(),
            input_names: InputNames::bert(),
            output_name: "last_hidden_state".to_string(),
            pooling: PoolingStrategy::Mean,
            approx_download_bytes: None,
            pad_id: 0,
        };
        let bs = extreme.embed_batch_size();
        assert!(
            (2..=256).contains(&bs),
            "embed_batch_size must clamp to [2, 256], got {}",
            bs
        );
    }

    // ===== TC-31: multi-model dim-threading (ModelConfig) =====

    #[test]
    fn tc31_resolve_config_dim_zero_falls_back_to_default() {
        let _lock = ENV_MUTEX.lock().unwrap();
        // TC-31.8: Custom config with dim=0 should be rejected and fall back to e5_base.
        std::env::remove_var("CQS_EMBEDDING_MODEL");
        let embedding_cfg = EmbeddingConfig {
            model: "zero-dim-model".to_string(),
            repo: Some("org/zero-dim".to_string()),
            onnx_path: None,
            tokenizer_path: None,
            dim: Some(0),
            max_seq_length: None,
            query_prefix: None,
            doc_prefix: None,
            input_names: None,
            output_name: None,
            pooling: None,
            pad_id: None,
        };
        let cfg = ModelConfig::resolve(None, Some(&embedding_cfg));
        let dm = ModelConfig::default_model();
        assert_eq!(cfg.name, dm.name, "dim=0 should cause fallback to default");
        assert_eq!(cfg.dim, dm.dim, "Fallback should have default model's dim");
    }

    // ===== TC-43: SEC-20 path traversal rejection tests =====

    #[test]
    fn test_sec20_onnx_path_traversal_rejected() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::remove_var("CQS_EMBEDDING_MODEL");
        let cfg = EmbeddingConfig {
            model: "evil-model".to_string(),
            repo: Some("evil/model".to_string()),
            onnx_path: Some("../../../etc/passwd".to_string()),
            tokenizer_path: None,
            dim: Some(768),
            max_seq_length: None,
            query_prefix: None,
            doc_prefix: None,
            input_names: None,
            output_name: None,
            pooling: None,
            pad_id: None,
        };
        let resolved = ModelConfig::resolve(None, Some(&cfg));
        assert_eq!(
            resolved.name,
            ModelConfig::default_model().name,
            "Traversal in onnx_path should fall back to default"
        );
    }

    #[test]
    fn test_sec20_tokenizer_path_traversal_rejected() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::remove_var("CQS_EMBEDDING_MODEL");
        let cfg = EmbeddingConfig {
            model: "evil-model".to_string(),
            repo: Some("evil/model".to_string()),
            onnx_path: Some("model.onnx".to_string()),
            tokenizer_path: Some("../../secret/tokenizer.json".to_string()),
            dim: Some(768),
            max_seq_length: None,
            query_prefix: None,
            doc_prefix: None,
            input_names: None,
            output_name: None,
            pooling: None,
            pad_id: None,
        };
        let resolved = ModelConfig::resolve(None, Some(&cfg));
        assert_eq!(
            resolved.name,
            ModelConfig::default_model().name,
            "Traversal in tokenizer_path should fall back to default"
        );
    }

    #[test]
    fn test_sec20_absolute_onnx_path_rejected() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::remove_var("CQS_EMBEDDING_MODEL");
        let cfg = EmbeddingConfig {
            model: "evil-model".to_string(),
            repo: Some("evil/model".to_string()),
            onnx_path: Some("/etc/passwd".to_string()),
            tokenizer_path: None,
            dim: Some(768),
            max_seq_length: None,
            query_prefix: None,
            doc_prefix: None,
            input_names: None,
            output_name: None,
            pooling: None,
            pad_id: None,
        };
        let resolved = ModelConfig::resolve(None, Some(&cfg));
        assert_eq!(
            resolved.name,
            ModelConfig::default_model().name,
            "Absolute onnx_path should fall back to default"
        );
    }

    #[test]
    fn test_sec20_valid_custom_paths_accepted() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::remove_var("CQS_EMBEDDING_MODEL");
        let cfg = EmbeddingConfig {
            model: "safe-model".to_string(),
            repo: Some("org/safe-model".to_string()),
            onnx_path: Some("onnx/model.onnx".to_string()),
            tokenizer_path: Some("tokenizer.json".to_string()),
            dim: Some(384),
            max_seq_length: None,
            query_prefix: None,
            doc_prefix: None,
            input_names: None,
            output_name: None,
            pooling: None,
            pad_id: None,
        };
        let resolved = ModelConfig::resolve(None, Some(&cfg));
        assert_eq!(
            resolved.name, "safe-model",
            "Valid paths should be accepted"
        );
        assert_eq!(resolved.onnx_path, "onnx/model.onnx");
        assert_eq!(resolved.tokenizer_path, "tokenizer.json");
    }

    #[test]
    fn test_sec20_dotdot_in_middle_rejected() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::remove_var("CQS_EMBEDDING_MODEL");
        let cfg = EmbeddingConfig {
            model: "tricky".to_string(),
            repo: Some("org/tricky".to_string()),
            onnx_path: Some("models/../../../etc/shadow".to_string()),
            tokenizer_path: None,
            dim: Some(768),
            max_seq_length: None,
            query_prefix: None,
            doc_prefix: None,
            input_names: None,
            output_name: None,
            pooling: None,
            pad_id: None,
        };
        let resolved = ModelConfig::resolve(None, Some(&cfg));
        assert_eq!(
            resolved.name,
            ModelConfig::default_model().name,
            ".. anywhere in path should fall back"
        );
    }

    // ===== SEC-28: repo validation tests =====

    #[test]
    fn test_sec28_repo_no_slash_rejected() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::remove_var("CQS_EMBEDDING_MODEL");
        let cfg = EmbeddingConfig {
            model: "bad-repo".to_string(),
            repo: Some("no-slash-repo".to_string()),
            onnx_path: None,
            tokenizer_path: None,
            dim: Some(768),
            max_seq_length: None,
            query_prefix: None,
            doc_prefix: None,
            input_names: None,
            output_name: None,
            pooling: None,
            pad_id: None,
        };
        let resolved = ModelConfig::resolve(None, Some(&cfg));
        assert_eq!(
            resolved.name,
            ModelConfig::default_model().name,
            "Repo without slash should fall back to default"
        );
    }

    #[test]
    fn test_sec28_repo_traversal_rejected() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::remove_var("CQS_EMBEDDING_MODEL");
        let cfg = EmbeddingConfig {
            model: "traversal-repo".to_string(),
            repo: Some("../../other-repo/model".to_string()),
            onnx_path: None,
            tokenizer_path: None,
            dim: Some(768),
            max_seq_length: None,
            query_prefix: None,
            doc_prefix: None,
            input_names: None,
            output_name: None,
            pooling: None,
            pad_id: None,
        };
        let resolved = ModelConfig::resolve(None, Some(&cfg));
        assert_eq!(
            resolved.name,
            ModelConfig::default_model().name,
            "Repo with .. should fall back to default"
        );
    }

    #[test]
    fn test_sec28_repo_absolute_path_rejected() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::remove_var("CQS_EMBEDDING_MODEL");
        let cfg = EmbeddingConfig {
            model: "abs-repo".to_string(),
            repo: Some("/etc/passwd/model".to_string()),
            onnx_path: None,
            tokenizer_path: None,
            dim: Some(768),
            max_seq_length: None,
            query_prefix: None,
            doc_prefix: None,
            input_names: None,
            output_name: None,
            pooling: None,
            pad_id: None,
        };
        let resolved = ModelConfig::resolve(None, Some(&cfg));
        assert_eq!(
            resolved.name,
            ModelConfig::default_model().name,
            "Repo starting with / should fall back to default"
        );
    }

    /// Consistency check: macro-generated constants must agree with `default_model()`.
    /// All four sources (`ModelConfig::DEFAULT_REPO`, `ModelConfig::DEFAULT_DIM`,
    /// `embedder::DEFAULT_MODEL_REPO`, `crate::EMBEDDING_DIM`) derive from the same
    /// `default = true` row in `define_embedder_presets!` — this test pins the
    /// invariant so a future preset table change is caught at test time.
    #[test]
    fn test_default_model_consts_consistent() {
        let dm = ModelConfig::default_model();
        assert_eq!(
            dm.repo,
            ModelConfig::DEFAULT_REPO,
            "ModelConfig::DEFAULT_REPO must match default_model().repo"
        );
        assert_eq!(
            dm.dim,
            ModelConfig::DEFAULT_DIM,
            "ModelConfig::DEFAULT_DIM must match default_model().dim"
        );
        assert_eq!(
            dm.repo,
            crate::embedder::DEFAULT_MODEL_REPO,
            "embedder::DEFAULT_MODEL_REPO mirror must match default_model().repo"
        );
        assert_eq!(
            dm.dim,
            crate::EMBEDDING_DIM,
            "EMBEDDING_DIM must match default_model().dim"
        );
    }

    /// Every preset in `define_embedder_presets!` must round-trip through
    /// `from_preset` by both short name AND repo ID.
    #[test]
    fn test_all_presets_roundtrip_from_preset() {
        for &name in ModelConfig::PRESET_NAMES {
            let cfg = ModelConfig::from_preset(name)
                .unwrap_or_else(|| panic!("PRESET_NAMES entry '{name}' must round-trip"));
            assert_eq!(
                cfg.name, name,
                "from_preset('{name}') must return cfg with matching short name"
            );
            // Repo ID must also resolve.
            let by_repo = ModelConfig::from_preset(&cfg.repo).unwrap_or_else(|| {
                panic!(
                    "from_preset('{}') (repo of {name}) must round-trip",
                    cfg.repo
                )
            });
            assert_eq!(by_repo.name, cfg.name);
        }
        assert_eq!(
            ModelConfig::PRESET_NAMES.len(),
            ModelConfig::PRESET_REPOS.len(),
            "PRESET_NAMES and PRESET_REPOS must agree by length"
        );
    }
}
