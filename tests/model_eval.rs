//! Model evaluation harness - embedding quality tests (ONNX via cqs Embedder)
//!
//! Run with: cargo test model_eval -- --ignored --nocapture
//!
//! Tests template variations, weight sweeps, type-aware embeddings, and reranker
//! quality using the cqs ONNX pipeline. For authoritative raw R@1 numbers across
//! models, use `run_hard_eval.py` in ~/training-data (SentenceTransformer, avoids
//! ONNX precision differences).
//!
//! CUDA gate: only BERT-style models (absolute position embeddings) are candidates.
//! Models with rotary embeddings (nomic, Qwen3) cause ort CPU fallback thrashing.

mod eval_common;

use cqs::parser::{Language, Parser};
use cqs::{generate_nl_description, generate_nl_with_template, NlTemplate};
use eval_common::{fixture_path, hard_fixture_path, EvalCase, EVAL_CASES, HARD_EVAL_CASES};
use ndarray::Array2;
use ort::session::Session;
use ort::value::Tensor;
use std::collections::HashMap;

/// Per-model evaluation results: (name, per-language hits/total, total hits, total queries)
type EvalResults<'a> = Vec<(&'a str, HashMap<Language, (usize, usize)>, usize, usize)>;

// ===== Model Configuration =====

struct ModelConfig {
    name: &'static str,
    /// HuggingFace repo ID or local directory path
    repo: &'static str,
    model_file: &'static str,
    tokenizer_file: &'static str,
    /// Prefix for document embeddings (None = no prefix)
    doc_prefix: Option<&'static str>,
    /// Prefix for query embeddings (None = no prefix)
    query_prefix: Option<&'static str>,
    /// Expected output dimension from model
    output_dim: usize,
    /// Max sequence length
    max_length: usize,
    /// Whether the model needs token_type_ids input
    needs_token_type_ids: bool,
    /// Output tensor name (most models use "last_hidden_state")
    output_tensor: &'static str,
    /// Pooling strategy
    pooling: Pooling,
}

#[derive(Clone, Copy)]
enum Pooling {
    /// Mean pooling over attention-masked tokens
    MeanPooling,
    /// Use [CLS] token embedding (first token)
    ClsToken,
}

const MODELS: &[ModelConfig] = &[
    ModelConfig {
        name: "E5-base-v2 (current)",
        repo: "intfloat/e5-base-v2",
        model_file: "onnx/model.onnx",
        tokenizer_file: "onnx/tokenizer.json",
        doc_prefix: Some("passage: "),
        query_prefix: Some("query: "),
        output_dim: 768,
        max_length: 512,
        needs_token_type_ids: true,
        output_tensor: "last_hidden_state",
        pooling: Pooling::MeanPooling,
    },
    ModelConfig {
        name: "BGE-base-en-v1.5",
        repo: "BAAI/bge-base-en-v1.5",
        model_file: "onnx/model.onnx",
        tokenizer_file: "tokenizer.json",
        doc_prefix: None,
        query_prefix: Some("Represent this sentence for searching relevant passages: "),
        output_dim: 768,
        max_length: 512,
        needs_token_type_ids: true,
        output_tensor: "last_hidden_state",
        pooling: Pooling::ClsToken,
    },
    ModelConfig {
        name: "E5-large-v2",
        repo: "intfloat/e5-large-v2",
        model_file: "onnx/model.onnx",
        tokenizer_file: "tokenizer.json",
        doc_prefix: Some("passage: "),
        query_prefix: Some("query: "),
        output_dim: 1024,
        max_length: 512,
        needs_token_type_ids: true,
        output_tensor: "last_hidden_state",
        pooling: Pooling::MeanPooling,
    },
    ModelConfig {
        name: "jina-v2-base-code",
        repo: "jinaai/jina-embeddings-v2-base-code",
        model_file: "onnx/model.onnx",
        tokenizer_file: "tokenizer.json",
        doc_prefix: None,
        query_prefix: None,
        output_dim: 768,
        max_length: 8192,
        needs_token_type_ids: false,
        output_tensor: "last_hidden_state",
        pooling: Pooling::MeanPooling,
    },
];

/// Local LoRA fine-tuned models (resolved at runtime via home dir)
fn local_lora_models() -> Vec<ModelConfig> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/user001".to_string());
    let mut models = Vec::new();

    let v5_dir = format!("{}/training-data/e5-code-search-lora-v5/onnx", home);
    if std::path::Path::new(&v5_dir).join("model.onnx").exists() {
        models.push(ModelConfig {
            name: "E5-LoRA-v5",
            repo: Box::leak(v5_dir.into_boxed_str()),
            model_file: "model.onnx",
            tokenizer_file: "tokenizer.json",
            doc_prefix: Some("passage: "),
            query_prefix: Some("query: "),
            output_dim: 768,
            max_length: 512,
            needs_token_type_ids: true,
            output_tensor: "last_hidden_state",
            pooling: Pooling::MeanPooling,
        });
    }

    let v7_dir = format!("{}/training-data/e5-code-search-lora-v7/onnx", home);
    if std::path::Path::new(&v7_dir).join("model.onnx").exists() {
        models.push(ModelConfig {
            name: "E5-LoRA-v7",
            repo: Box::leak(v7_dir.into_boxed_str()),
            model_file: "model.onnx",
            tokenizer_file: "tokenizer.json",
            doc_prefix: Some("passage: "),
            query_prefix: Some("query: "),
            output_dim: 768,
            max_length: 512,
            needs_token_type_ids: true,
            pooling: Pooling::MeanPooling,
            output_tensor: "last_hidden_state",
        });
    }

    let v7b_dir = format!("{}/training-data/e5-code-search-lora-v7b/onnx", home);
    if std::path::Path::new(&v7b_dir).join("model.onnx").exists() {
        models.push(ModelConfig {
            name: "E5-LoRA-v7b",
            repo: Box::leak(v7b_dir.into_boxed_str()),
            model_file: "model.onnx",
            tokenizer_file: "tokenizer.json",
            doc_prefix: Some("passage: "),
            query_prefix: Some("query: "),
            output_dim: 768,
            max_length: 512,
            needs_token_type_ids: true,
            pooling: Pooling::MeanPooling,
            output_tensor: "last_hidden_state",
        });
    }

    let v8_dir = format!("{}/training-data/e5-code-search-lora-v8-keydac/onnx", home);
    if std::path::Path::new(&v8_dir).join("model.onnx").exists() {
        models.push(ModelConfig {
            name: "E5-LoRA-v8-keydac",
            repo: Box::leak(v8_dir.into_boxed_str()),
            model_file: "model.onnx",
            tokenizer_file: "tokenizer.json",
            doc_prefix: Some("passage: "),
            query_prefix: Some("query: "),
            output_dim: 768,
            max_length: 512,
            needs_token_type_ids: true,
            pooling: Pooling::MeanPooling,
            output_tensor: "last_hidden_state",
        });
    }

    models
}

// EvalCase, EVAL_CASES, HARD_EVAL_CASES, fixture_path, hard_fixture_path imported from eval_common

// ===== Eval Embedder (model-agnostic) =====

struct EvalEmbedder<'a> {
    session: Session,
    tokenizer: tokenizers::Tokenizer,
    config: &'a ModelConfig,
}

impl<'a> EvalEmbedder<'a> {
    fn new(config: &'a ModelConfig) -> Result<Self, Box<dyn std::error::Error>> {
        let local_path = std::path::Path::new(config.repo);
        let (model_path, tokenizer_path) = if local_path.is_dir() {
            // Local model directory
            eprintln!("  Loading {} from local: {}...", config.name, config.repo);
            let mp = local_path.join(config.model_file);
            let tp = local_path.join(config.tokenizer_file);
            if !mp.exists() {
                return Err(format!("Model file not found: {}", mp.display()).into());
            }
            if !tp.exists() {
                return Err(format!("Tokenizer not found: {}", tp.display()).into());
            }
            (mp, tp)
        } else {
            // HuggingFace Hub download
            use hf_hub::api::sync::Api;
            eprintln!("  Downloading {} from {}...", config.name, config.repo);
            let api = Api::new()?;
            let repo = api.model(config.repo.to_string());
            (
                repo.get(config.model_file)?,
                repo.get(config.tokenizer_file)?,
            )
        };

        eprintln!("  Creating ONNX session...");
        let session = Session::builder()?.commit_from_file(&model_path)?;

        let tokenizer = tokenizers::Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| format!("Tokenizer error: {}", e))?;

        Ok(Self {
            session,
            tokenizer,
            config,
        })
    }

    /// Embed a batch of texts, returning raw model-dim vectors (no sentiment)
    fn embed_batch(
        &mut self,
        texts: &[String],
    ) -> Result<Vec<Vec<f32>>, Box<dyn std::error::Error>> {
        if texts.is_empty() {
            return Ok(vec![]);
        }

        // Tokenize
        let encodings = self
            .tokenizer
            .encode_batch(texts.to_vec(), true)
            .map_err(|e| format!("Tokenizer error: {}", e))?;

        // Prepare inputs
        let input_ids: Vec<Vec<i64>> = encodings
            .iter()
            .map(|e| e.get_ids().iter().map(|&id| id as i64).collect())
            .collect();
        let attention_mask: Vec<Vec<i64>> = encodings
            .iter()
            .map(|e| e.get_attention_mask().iter().map(|&m| m as i64).collect())
            .collect();

        let max_len = input_ids
            .iter()
            .map(|v| v.len())
            .max()
            .unwrap_or(0)
            .min(self.config.max_length);

        let batch_size = texts.len();
        let input_ids_arr = pad_2d_i64(&input_ids, max_len, 0);
        let attention_mask_arr = pad_2d_i64(&attention_mask, max_len, 0);

        let input_ids_tensor = Tensor::from_array(input_ids_arr)?;
        let attention_mask_tensor = Tensor::from_array(attention_mask_arr)?;

        // Run inference
        let outputs = if self.config.needs_token_type_ids {
            let token_type_ids_arr = Array2::<i64>::zeros((batch_size, max_len));
            let token_type_ids_tensor = Tensor::from_array(token_type_ids_arr)?;
            self.session.run(ort::inputs![
                "input_ids" => input_ids_tensor,
                "attention_mask" => attention_mask_tensor,
                "token_type_ids" => token_type_ids_tensor,
            ])?
        } else {
            self.session.run(ort::inputs![
                "input_ids" => input_ids_tensor,
                "attention_mask" => attention_mask_tensor,
            ])?
        };

        // Extract embeddings
        let (_shape, data) = outputs[self.config.output_tensor].try_extract_tensor::<f32>()?;

        let embedding_dim = self.config.output_dim;
        let seq_len = max_len;
        let mut results = Vec::with_capacity(batch_size);

        for (i, mask_vec) in attention_mask.iter().enumerate().take(batch_size) {
            let embedding = match self.config.pooling {
                Pooling::MeanPooling => {
                    let mut sum = vec![0.0f32; embedding_dim];
                    let mut count = 0.0f32;

                    for j in 0..seq_len {
                        let mask = mask_vec.get(j).copied().unwrap_or(0) as f32;
                        if mask > 0.0 {
                            count += mask;
                            let offset = i * seq_len * embedding_dim + j * embedding_dim;
                            for (k, sum_val) in sum.iter_mut().enumerate() {
                                *sum_val += data[offset + k] * mask;
                            }
                        }
                    }
                    if count > 0.0 {
                        for val in &mut sum {
                            *val /= count;
                        }
                    }
                    sum
                }
                Pooling::ClsToken => {
                    let offset = i * seq_len * embedding_dim;
                    data[offset..offset + embedding_dim].to_vec()
                }
            };

            results.push(normalize_l2(embedding));
        }

        Ok(results)
    }

    /// Embed documents with model-specific prefix
    fn embed_documents(
        &mut self,
        texts: &[&str],
    ) -> Result<Vec<Vec<f32>>, Box<dyn std::error::Error>> {
        let prefixed: Vec<String> = texts
            .iter()
            .map(|t| match self.config.doc_prefix {
                Some(prefix) => format!("{}{}", prefix, t),
                None => t.to_string(),
            })
            .collect();
        self.embed_batch(&prefixed)
    }

    /// Embed a single query with model-specific prefix
    fn embed_query(&mut self, text: &str) -> Result<Vec<f32>, Box<dyn std::error::Error>> {
        let prefixed = match self.config.query_prefix {
            Some(prefix) => format!("{}{}", prefix, text),
            None => text.to_string(),
        };
        let results = self.embed_batch(&[prefixed])?;
        Ok(results.into_iter().next().unwrap())
    }
}

// ===== Utility functions =====

fn pad_2d_i64(inputs: &[Vec<i64>], max_len: usize, pad_value: i64) -> Array2<i64> {
    let batch_size = inputs.len();
    let mut arr = Array2::from_elem((batch_size, max_len), pad_value);
    for (i, seq) in inputs.iter().enumerate() {
        for (j, &val) in seq.iter().take(max_len).enumerate() {
            arr[[i, j]] = val;
        }
    }
    arr
}

fn normalize_l2(mut v: Vec<f32>) -> Vec<f32> {
    let norm_sq: f32 = v.iter().fold(0.0, |acc, &x| acc + x * x);
    if norm_sq > 0.0 {
        let inv_norm = 1.0 / norm_sq.sqrt();
        v.iter_mut().for_each(|x| *x *= inv_norm);
    }
    v
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    // Both are L2-normalized, so cosine similarity = dot product
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

// ===== Chunk with embedding =====

struct IndexedChunk {
    name: String,
    language: Language,
    embedding: Vec<f32>,
}

// ===== Main eval test =====

#[test]
#[ignore] // Slow - downloads models. Run with: cargo test model_eval -- --ignored --nocapture
fn test_model_comparison() {
    let parser = Parser::new().expect("Failed to initialize parser");

    // Parse all fixtures and generate NL descriptions
    eprintln!("Parsing fixtures and generating NL descriptions...");
    let languages = [
        Language::Rust,
        Language::Python,
        Language::TypeScript,
        Language::JavaScript,
        Language::Go,
    ];

    struct ChunkDesc {
        name: String,
        language: Language,
        nl_text: String,
    }

    let mut chunk_descs: Vec<ChunkDesc> = Vec::new();
    for lang in &languages {
        let path = fixture_path(*lang);
        let chunks = parser.parse_file(&path).expect("Failed to parse fixture");
        for chunk in &chunks {
            let nl = generate_nl_description(chunk);
            chunk_descs.push(ChunkDesc {
                name: chunk.name.clone(),
                language: *lang,
                nl_text: nl,
            });
        }
    }
    eprintln!("  {} chunks with NL descriptions\n", chunk_descs.len());

    // Evaluate each model
    eprintln!("=== Model Comparison ===\n");

    let mut all_results: EvalResults = Vec::new();

    for model_config in MODELS {
        eprintln!("--- {} ---", model_config.name);

        let mut embedder = match EvalEmbedder::new(model_config) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("  SKIP: Failed to load model: {}\n", e);
                continue;
            }
        };

        // Embed all chunk descriptions
        eprintln!("  Embedding {} chunks...", chunk_descs.len());
        let nl_texts: Vec<&str> = chunk_descs.iter().map(|c| c.nl_text.as_str()).collect();

        // Batch embed in groups of 16
        let mut all_embeddings: Vec<Vec<f32>> = Vec::new();
        for batch in nl_texts.chunks(16) {
            match embedder.embed_documents(batch) {
                Ok(embs) => all_embeddings.extend(embs),
                Err(e) => {
                    eprintln!("  SKIP: Embedding failed: {}\n", e);
                    continue;
                }
            }
        }

        if all_embeddings.len() != chunk_descs.len() {
            eprintln!("  SKIP: Embedding count mismatch\n");
            continue;
        }

        // Build indexed chunks
        let indexed: Vec<IndexedChunk> = chunk_descs
            .iter()
            .zip(all_embeddings.into_iter())
            .map(|(desc, emb)| IndexedChunk {
                name: desc.name.clone(),
                language: desc.language,
                embedding: emb,
            })
            .collect();

        // Run eval cases
        let mut results_by_lang: HashMap<Language, (usize, usize)> = HashMap::new();
        let mut total_hits = 0;
        let mut total_cases = 0;

        for case in EVAL_CASES {
            let query_embedding = match embedder.embed_query(case.query) {
                Ok(e) => e,
                Err(e) => {
                    eprintln!("  Query embed failed: {}", e);
                    continue;
                }
            };

            // Find top-5 by cosine similarity, filtered by language
            let mut scored: Vec<(&str, f32)> = indexed
                .iter()
                .filter(|c| c.language == case.language)
                .map(|c| {
                    (
                        c.name.as_str(),
                        cosine_similarity(&query_embedding, &c.embedding),
                    )
                })
                .collect();
            scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
            scored.truncate(5);

            let found = scored.iter().any(|(name, _)| *name == case.expected_name);

            let (hits, total) = results_by_lang.entry(case.language).or_insert((0, 0));
            *total += 1;
            if found {
                *hits += 1;
                total_hits += 1;
            }
            total_cases += 1;

            let status = if found { "+" } else { "-" };
            let top_names: Vec<&str> = scored.iter().take(3).map(|(n, _)| *n).collect();
            eprintln!(
                "  {} [{:?}] \"{}\" -> exp: {}, got: {:?}",
                status, case.language, case.query, case.expected_name, top_names
            );
        }

        // Print per-language results
        eprintln!();
        for lang in &languages {
            if let Some((hits, total)) = results_by_lang.get(lang) {
                let pct = (*hits as f64 / *total as f64) * 100.0;
                eprintln!("  {:?}: {}/{} ({:.0}%)", lang, hits, total, pct);
            }
        }
        let overall_pct = if total_cases > 0 {
            (total_hits as f64 / total_cases as f64) * 100.0
        } else {
            0.0
        };
        eprintln!(
            "  Overall: {}/{} ({:.0}%)\n",
            total_hits, total_cases, overall_pct
        );

        all_results.push((model_config.name, results_by_lang, total_hits, total_cases));
    }

    // Print comparison table
    eprintln!("=== Comparison Table ===\n");
    eprintln!(
        "{:<25} {:>6} {:>6} {:>6} {:>6} {:>6} {:>8}",
        "Model", "Rust", "Py", "TS", "JS", "Go", "Overall"
    );
    eprintln!("{}", "-".repeat(75));

    for (name, by_lang, total_hits, total_cases) in &all_results {
        let mut row = format!("{:<25}", name);
        for lang in &languages {
            if let Some((hits, total)) = by_lang.get(lang) {
                row += &format!(" {:>5}/{}", hits, total);
            } else {
                row += "    n/a";
            }
        }
        let pct = if *total_cases > 0 {
            (*total_hits as f64 / *total_cases as f64) * 100.0
        } else {
            0.0
        };
        row += &format!(" {:>6.0}%", pct);
        eprintln!("{}", row);
    }
    eprintln!();
}

// ===== Template comparison eval =====

#[test]
#[ignore] // Slow - embeds 5x. Run with: cargo test template_eval -- --ignored --nocapture
fn test_template_comparison() {
    let parser = Parser::new().expect("Failed to initialize parser");
    let e5_config = &MODELS[0]; // E5-base-v2
    let mut embedder = EvalEmbedder::new(e5_config).expect("Failed to load E5-base-v2");

    let languages = [
        Language::Rust,
        Language::Python,
        Language::TypeScript,
        Language::JavaScript,
        Language::Go,
    ];

    // Parse fixtures once
    let mut chunks: Vec<cqs::parser::Chunk> = Vec::new();
    for lang in &languages {
        let path = fixture_path(*lang);
        let parsed = parser.parse_file(&path).expect("Failed to parse fixture");
        chunks.extend(parsed);
    }
    eprintln!("Parsed {} chunks from fixtures\n", chunks.len());

    let templates = [
        ("Compact", NlTemplate::Compact),
        ("DocFirst", NlTemplate::DocFirst),
    ];

    let mut all_results: EvalResults = Vec::new();

    for (template_name, template) in &templates {
        eprintln!("--- {} ---", template_name);

        // Generate NL descriptions with this template
        let nl_texts: Vec<String> = chunks
            .iter()
            .map(|c| generate_nl_with_template(c, *template))
            .collect();

        // Show a sample
        if let Some(first) = nl_texts.first() {
            eprintln!("  Sample: {}", &first[..first.len().min(120)]);
        }

        // Embed all descriptions
        let text_refs: Vec<&str> = nl_texts.iter().map(|s| s.as_str()).collect();
        let mut all_embeddings: Vec<Vec<f32>> = Vec::new();
        for batch in text_refs.chunks(16) {
            match embedder.embed_documents(batch) {
                Ok(embs) => all_embeddings.extend(embs),
                Err(e) => {
                    eprintln!("  SKIP: Embedding failed: {}\n", e);
                    continue;
                }
            }
        }

        if all_embeddings.len() != chunks.len() {
            eprintln!("  SKIP: Embedding count mismatch\n");
            continue;
        }

        // Build indexed chunks
        let indexed: Vec<IndexedChunk> = chunks
            .iter()
            .zip(all_embeddings.into_iter())
            .map(|(chunk, emb)| IndexedChunk {
                name: chunk.name.clone(),
                language: chunk.language,
                embedding: emb,
            })
            .collect();

        // Run eval cases
        let mut results_by_lang: HashMap<Language, (usize, usize)> = HashMap::new();
        let mut total_hits = 0;
        let mut total_cases = 0;

        for case in EVAL_CASES {
            let query_embedding = embedder
                .embed_query(case.query)
                .expect("Query embed failed");

            let mut scored: Vec<(&str, f32)> = indexed
                .iter()
                .filter(|c| c.language == case.language)
                .map(|c| {
                    (
                        c.name.as_str(),
                        cosine_similarity(&query_embedding, &c.embedding),
                    )
                })
                .collect();
            scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
            scored.truncate(5);

            let found = scored.iter().any(|(name, _)| *name == case.expected_name);

            let (hits, total) = results_by_lang.entry(case.language).or_insert((0, 0));
            *total += 1;
            if found {
                *hits += 1;
                total_hits += 1;
            }
            total_cases += 1;

            if !found {
                let top_names: Vec<&str> = scored.iter().take(3).map(|(n, _)| *n).collect();
                eprintln!(
                    "  MISS [{:?}] \"{}\" -> exp: {}, got: {:?}",
                    case.language, case.query, case.expected_name, top_names
                );
            }
        }

        // Per-language summary
        for lang in &languages {
            if let Some((hits, total)) = results_by_lang.get(lang) {
                let pct = (*hits as f64 / *total as f64) * 100.0;
                eprintln!("  {:?}: {}/{} ({:.0}%)", lang, hits, total, pct);
            }
        }
        let overall_pct = if total_cases > 0 {
            (total_hits as f64 / total_cases as f64) * 100.0
        } else {
            0.0
        };
        eprintln!(
            "  Overall: {}/{} ({:.0}%)\n",
            total_hits, total_cases, overall_pct
        );

        all_results.push((template_name, results_by_lang, total_hits, total_cases));
    }

    // Print comparison table
    eprintln!("=== Template Comparison ===\n");
    eprintln!(
        "{:<25} {:>6} {:>6} {:>6} {:>6} {:>6} {:>8}",
        "Template", "Rust", "Py", "TS", "JS", "Go", "Overall"
    );
    eprintln!("{}", "-".repeat(75));

    for (name, by_lang, total_hits, total_cases) in &all_results {
        let mut row = format!("{:<25}", name);
        for lang in &languages {
            if let Some((hits, total)) = by_lang.get(lang) {
                row += &format!(" {:>5}/{}", hits, total);
            } else {
                row += "    n/a";
            }
        }
        let pct = if *total_cases > 0 {
            (*total_hits as f64 / *total_cases as f64) * 100.0
        } else {
            0.0
        };
        row += &format!(" {:>6.0}%", pct);
        eprintln!("{}", row);
    }
    eprintln!();
}

// ===== Hard template comparison =====

#[test]
#[ignore] // Slow - embeds 9x on hard corpus. Run with: cargo test hard_template -- --ignored --nocapture
fn test_hard_template_comparison() {
    let parser = Parser::new().expect("Failed to initialize parser");
    let e5_config = &MODELS[0]; // E5-base-v2
    let mut embedder = EvalEmbedder::new(e5_config).expect("Failed to load E5-base-v2");

    let languages = [
        Language::Rust,
        Language::Python,
        Language::TypeScript,
        Language::JavaScript,
        Language::Go,
    ];

    // Parse BOTH original and hard fixtures — combined corpus
    let mut chunks: Vec<cqs::parser::Chunk> = Vec::new();
    for lang in &languages {
        let path = fixture_path(*lang);
        let parsed = parser
            .parse_file(&path)
            .expect("Failed to parse original fixture");
        chunks.extend(parsed);
        let hard_path = hard_fixture_path(*lang);
        if hard_path.exists() {
            let parsed = parser
                .parse_file(&hard_path)
                .expect("Failed to parse hard fixture");
            chunks.extend(parsed);
        }
    }
    eprintln!(
        "Parsed {} chunks from original + hard fixtures\n",
        chunks.len()
    );

    let templates = [
        ("Compact", NlTemplate::Compact),
        ("DocFirst", NlTemplate::DocFirst),
    ];

    // Store results: (name, recall@1, recall@5, mrr, ndcg@10)
    let mut all_results: Vec<(&str, f64, f64, f64, f64)> = Vec::new();

    for (template_name, template) in &templates {
        eprintln!("--- {} ---", template_name);

        // Generate NL descriptions with this template
        let nl_texts: Vec<String> = chunks
            .iter()
            .map(|c| generate_nl_with_template(c, *template))
            .collect();

        if let Some(first) = nl_texts.first() {
            eprintln!("  Sample: {}", &first[..first.len().min(120)]);
        }

        // Embed all descriptions
        let text_refs: Vec<&str> = nl_texts.iter().map(|s| s.as_str()).collect();
        let mut all_embeddings: Vec<Vec<f32>> = Vec::new();
        for batch in text_refs.chunks(16) {
            match embedder.embed_documents(batch) {
                Ok(embs) => all_embeddings.extend(embs),
                Err(e) => {
                    eprintln!("  SKIP: Embedding failed: {}\n", e);
                    continue;
                }
            }
        }

        if all_embeddings.len() != chunks.len() {
            eprintln!("  SKIP: Embedding count mismatch\n");
            continue;
        }

        let indexed: Vec<IndexedChunk> = chunks
            .iter()
            .zip(all_embeddings.into_iter())
            .map(|(chunk, emb)| IndexedChunk {
                name: chunk.name.clone(),
                language: chunk.language,
                embedding: emb,
            })
            .collect();

        // Pre-embed all queries
        let query_embeddings: Vec<Vec<f32>> = HARD_EVAL_CASES
            .iter()
            .map(|case| {
                embedder
                    .embed_query(case.query)
                    .expect("Query embed failed")
            })
            .collect();

        // Compute metrics
        let (r1_hits, r1_total) =
            compute_recall_at_k(&indexed, HARD_EVAL_CASES, &query_embeddings, 1);
        let (r5_hits, r5_total) =
            compute_recall_at_k(&indexed, HARD_EVAL_CASES, &query_embeddings, 5);
        let (mrr, per_lang_mrr) = compute_mrr(&indexed, HARD_EVAL_CASES, &query_embeddings);
        let ndcg_10 = compute_ndcg_at_k(&indexed, HARD_EVAL_CASES, &query_embeddings, 10);

        let recall_1 = r1_hits as f64 / r1_total as f64;
        let recall_5 = r5_hits as f64 / r5_total as f64;

        eprintln!("  R@1: {}/{} ({:.1}%)", r1_hits, r1_total, recall_1 * 100.0);
        eprintln!("  R@5: {}/{} ({:.1}%)", r5_hits, r5_total, recall_5 * 100.0);
        eprintln!("  MRR: {:.4}", mrr);
        eprintln!("  NDCG@10: {:.4}", ndcg_10);

        // Per-language detail
        for (lang, lang_mrr, count) in &per_lang_mrr {
            eprintln!("    {:?}: MRR {:.4} ({} queries)", lang, lang_mrr, count);
        }
        eprintln!();

        all_results.push((template_name, recall_1, recall_5, mrr, ndcg_10));
    }

    // Print comparison table
    eprintln!("=== Hard Template Comparison (55 queries, confusable corpus) ===\n");
    eprintln!(
        "{:<25} {:>10} {:>10} {:>10} {:>10}",
        "Template", "Recall@1", "Recall@5", "MRR", "NDCG@10"
    );
    eprintln!("{}", "-".repeat(70));
    for (name, r1, r5, mrr, ndcg) in &all_results {
        eprintln!(
            "{:<25} {:>9.1}% {:>9.1}% {:>10.4} {:>10.4}",
            name,
            r1 * 100.0,
            r5 * 100.0,
            mrr,
            ndcg
        );
    }
    eprintln!();
}

// ===== Hard eval - confusable functions =====

/// Hard eval cases imported from eval_common — see eval_common::HARD_EVAL_CASES

/// Compute Mean Reciprocal Rank from ranked results using pre-computed query embeddings
fn compute_mrr(
    indexed: &[IndexedChunk],
    cases: &[EvalCase],
    query_embeddings: &[Vec<f32>],
) -> (f64, Vec<(Language, f64, usize)>) {
    let languages = [
        Language::Rust,
        Language::Python,
        Language::TypeScript,
        Language::JavaScript,
        Language::Go,
    ];

    let mut total_rr = 0.0;
    let mut total_count = 0;
    let mut lang_rr: HashMap<Language, (f64, usize)> = HashMap::new();

    for (case, query_embedding) in cases.iter().zip(query_embeddings.iter()) {
        let mut scored: Vec<(&str, f32)> = indexed
            .iter()
            .filter(|c| c.language == case.language)
            .map(|c| {
                (
                    c.name.as_str(),
                    cosine_similarity(query_embedding, &c.embedding),
                )
            })
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());

        // Find rank of expected result (1-indexed)
        let rank = scored
            .iter()
            .position(|(name, _)| *name == case.expected_name)
            .map(|pos| pos + 1);

        let rr = match rank {
            Some(r) => 1.0 / r as f64,
            None => 0.0,
        };

        total_rr += rr;
        total_count += 1;

        let entry = lang_rr.entry(case.language).or_insert((0.0, 0));
        entry.0 += rr;
        entry.1 += 1;
    }

    let mrr = if total_count > 0 {
        total_rr / total_count as f64
    } else {
        0.0
    };

    let per_lang: Vec<(Language, f64, usize)> = languages
        .iter()
        .filter_map(|lang| {
            lang_rr.get(lang).map(|(rr, count)| {
                (
                    *lang,
                    if *count > 0 { rr / *count as f64 } else { 0.0 },
                    *count,
                )
            })
        })
        .collect();

    (mrr, per_lang)
}

/// Per-model hard eval results: (name, recall@1, recall@3, recall@5, recall@10, MRR, NDCG@10, per-lang MRR)
type HardEvalResults<'a> = Vec<(
    &'a str,
    f64,
    f64,
    f64,
    f64,
    f64,
    f64,
    Vec<(Language, f64, usize)>,
)>;

/// Compute recall at K for given cases using pre-computed query embeddings
fn compute_recall_at_k(
    indexed: &[IndexedChunk],
    cases: &[EvalCase],
    query_embeddings: &[Vec<f32>],
    k: usize,
) -> (usize, usize) {
    let mut hits = 0;
    let mut total = 0;

    for (case, query_embedding) in cases.iter().zip(query_embeddings.iter()) {
        let mut scored: Vec<(&str, f32)> = indexed
            .iter()
            .filter(|c| c.language == case.language)
            .map(|c| {
                (
                    c.name.as_str(),
                    cosine_similarity(query_embedding, &c.embedding),
                )
            })
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        scored.truncate(k);

        if scored.iter().any(|(name, _)| *name == case.expected_name) {
            hits += 1;
        }
        total += 1;
    }

    (hits, total)
}

/// Compute NDCG@k for single-relevant-document queries.
///
/// For single-relevant queries, DCG@k = 1/log2(rank+1) if found in top-k, else 0.
/// IDCG = 1/log2(2) = 1.0 (perfect = rank 1). So NDCG@k = 1/log2(rank+1) if found.
fn compute_ndcg_at_k(
    indexed: &[IndexedChunk],
    cases: &[EvalCase],
    query_embeddings: &[Vec<f32>],
    k: usize,
) -> f64 {
    let mut total_ndcg = 0.0;
    let mut count = 0;

    for (case, query_embedding) in cases.iter().zip(query_embeddings.iter()) {
        let mut scored: Vec<(&str, f32)> = indexed
            .iter()
            .filter(|c| c.language == case.language)
            .map(|c| {
                (
                    c.name.as_str(),
                    cosine_similarity(query_embedding, &c.embedding),
                )
            })
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        scored.truncate(k);

        let rank = scored
            .iter()
            .position(|(name, _)| *name == case.expected_name)
            .map(|pos| pos + 1);

        // For single-relevant: NDCG = DCG / IDCG = (1/log2(rank+1)) / (1/log2(2)) = 1/log2(rank+1)
        let ndcg = match rank {
            Some(r) => 1.0 / (r as f64 + 1.0).log2(),
            None => 0.0,
        };

        total_ndcg += ndcg;
        count += 1;
    }

    if count > 0 {
        total_ndcg / count as f64
    } else {
        0.0
    }
}

#[test]
#[ignore] // Slow - downloads models. Run with: cargo test hard_model -- --ignored --nocapture
fn test_hard_model_comparison() {
    let parser = Parser::new().expect("Failed to initialize parser");

    let languages = [
        Language::Rust,
        Language::Python,
        Language::TypeScript,
        Language::JavaScript,
        Language::Go,
    ];

    // Parse BOTH original and hard fixtures — combined corpus
    struct ChunkDesc {
        name: String,
        language: Language,
        nl_text: String,
    }

    let mut chunk_descs: Vec<ChunkDesc> = Vec::new();
    for lang in &languages {
        // Original fixtures
        let path = fixture_path(*lang);
        let chunks = parser
            .parse_file(&path)
            .expect("Failed to parse original fixture");
        for chunk in &chunks {
            let nl = generate_nl_description(chunk);
            chunk_descs.push(ChunkDesc {
                name: chunk.name.clone(),
                language: *lang,
                nl_text: nl,
            });
        }
        // Hard fixtures (confusable functions)
        let hard_path = hard_fixture_path(*lang);
        if hard_path.exists() {
            let chunks = parser
                .parse_file(&hard_path)
                .expect("Failed to parse hard fixture");
            for chunk in &chunks {
                let nl = generate_nl_description(chunk);
                chunk_descs.push(ChunkDesc {
                    name: chunk.name.clone(),
                    language: *lang,
                    nl_text: nl,
                });
            }
        }
    }
    eprintln!(
        "Parsed {} chunks from original + hard fixtures\n",
        chunk_descs.len()
    );

    // HF models: E5-base-v2 and jina-v2-base-code
    let test_models: Vec<&ModelConfig> = vec![&MODELS[0], &MODELS[3]];

    // Local LoRA models (if present)
    let local_models = local_lora_models();
    let local_refs: Vec<&ModelConfig> = local_models.iter().collect();

    let all_models: Vec<&ModelConfig> = test_models.into_iter().chain(local_refs).collect();

    let mut all_results: HardEvalResults = Vec::new();

    for model_config in all_models {
        eprintln!("--- {} ---", model_config.name);

        let mut embedder = match EvalEmbedder::new(model_config) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("  SKIP: Failed to load model: {}\n", e);
                continue;
            }
        };

        // Embed all chunk descriptions
        eprintln!("  Embedding {} chunks...", chunk_descs.len());
        let nl_texts: Vec<&str> = chunk_descs.iter().map(|c| c.nl_text.as_str()).collect();

        let mut all_embeddings: Vec<Vec<f32>> = Vec::new();
        for batch in nl_texts.chunks(16) {
            match embedder.embed_documents(batch) {
                Ok(embs) => all_embeddings.extend(embs),
                Err(e) => {
                    eprintln!("  SKIP: Embedding failed: {}\n", e);
                    continue;
                }
            }
        }

        if all_embeddings.len() != chunk_descs.len() {
            eprintln!("  SKIP: Embedding count mismatch\n");
            continue;
        }

        let indexed: Vec<IndexedChunk> = chunk_descs
            .iter()
            .zip(all_embeddings.into_iter())
            .map(|(desc, emb)| IndexedChunk {
                name: desc.name.clone(),
                language: desc.language,
                embedding: emb,
            })
            .collect();

        // Pre-embed all queries once (eliminates 4x redundant ONNX inference)
        eprintln!("  Embedding {} queries...", HARD_EVAL_CASES.len());
        let query_embeddings: Vec<Vec<f32>> = HARD_EVAL_CASES
            .iter()
            .map(|case| {
                embedder
                    .embed_query(case.query)
                    .expect("Query embed failed")
            })
            .collect();

        // Run hard eval cases with detailed output
        eprintln!("\n  Hard eval cases ({} queries):", HARD_EVAL_CASES.len());
        for (case, query_embedding) in HARD_EVAL_CASES.iter().zip(query_embeddings.iter()) {
            let mut scored: Vec<(&str, f32)> = indexed
                .iter()
                .filter(|c| c.language == case.language)
                .map(|c| {
                    (
                        c.name.as_str(),
                        cosine_similarity(query_embedding, &c.embedding),
                    )
                })
                .collect();
            scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());

            let rank = scored
                .iter()
                .position(|(name, _)| *name == case.expected_name)
                .map(|pos| pos + 1);

            let top5: Vec<&str> = scored.iter().take(5).map(|(n, _)| *n).collect();
            let status = match rank {
                Some(1) => "+",
                Some(r) if r <= 5 => "~",
                _ => "-",
            };
            eprintln!(
                "  {} [{:?}] \"{}\" -> exp: {} (rank: {}), top5: {:?}",
                status,
                case.language,
                case.query,
                case.expected_name,
                rank.map(|r| r.to_string()).unwrap_or("miss".to_string()),
                top5
            );
        }

        // Compute metrics (reuses pre-embedded queries — no redundant inference)
        let (r1_hits, r1_total) =
            compute_recall_at_k(&indexed, HARD_EVAL_CASES, &query_embeddings, 1);
        let (r3_hits, r3_total) =
            compute_recall_at_k(&indexed, HARD_EVAL_CASES, &query_embeddings, 3);
        let (r5_hits, r5_total) =
            compute_recall_at_k(&indexed, HARD_EVAL_CASES, &query_embeddings, 5);
        let (r10_hits, r10_total) =
            compute_recall_at_k(&indexed, HARD_EVAL_CASES, &query_embeddings, 10);
        let (mrr, per_lang_mrr) = compute_mrr(&indexed, HARD_EVAL_CASES, &query_embeddings);
        let ndcg_10 = compute_ndcg_at_k(&indexed, HARD_EVAL_CASES, &query_embeddings, 10);

        let recall_1 = r1_hits as f64 / r1_total as f64;
        let recall_3 = r3_hits as f64 / r3_total as f64;
        let recall_5 = r5_hits as f64 / r5_total as f64;
        let recall_10 = r10_hits as f64 / r10_total as f64;

        eprintln!(
            "\n  Recall@1: {}/{} ({:.1}%)",
            r1_hits,
            r1_total,
            recall_1 * 100.0
        );
        eprintln!(
            "  Recall@3: {}/{} ({:.1}%)",
            r3_hits,
            r3_total,
            recall_3 * 100.0
        );
        eprintln!(
            "  Recall@5: {}/{} ({:.1}%)",
            r5_hits,
            r5_total,
            recall_5 * 100.0
        );
        eprintln!(
            "  Recall@10: {}/{} ({:.1}%)",
            r10_hits,
            r10_total,
            recall_10 * 100.0
        );
        eprintln!("  MRR: {:.4}", mrr);
        eprintln!("  NDCG@10: {:.4}", ndcg_10);

        eprintln!("\n  Per-language MRR:");
        for (lang, lang_mrr, count) in &per_lang_mrr {
            eprintln!("    {:?}: {:.4} ({} queries)", lang, lang_mrr, count);
        }
        eprintln!();

        all_results.push((
            model_config.name,
            recall_1,
            recall_3,
            recall_5,
            recall_10,
            mrr,
            ndcg_10,
            per_lang_mrr,
        ));
    }

    // Print comparison table
    eprintln!("=== Hard Eval Comparison ===\n");
    eprintln!(
        "{:<25} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10}",
        "Model", "Recall@1", "Recall@5", "Recall@10", "MRR", "NDCG@10", "Recall@3"
    );
    eprintln!("{}", "-".repeat(100));
    for (name, r1, r3, r5, r10, mrr, ndcg, _) in &all_results {
        eprintln!(
            "{:<25} {:>9.1}% {:>9.1}% {:>9.1}% {:>10.4} {:>10.4} {:>9.1}%",
            name,
            r1 * 100.0,
            r5 * 100.0,
            r10 * 100.0,
            mrr,
            ndcg,
            r3 * 100.0
        );
    }
    eprintln!();

    // Per-language MRR comparison
    eprintln!("=== Per-Language MRR ===\n");
    eprintln!(
        "{:<25} {:>8} {:>8} {:>8} {:>8} {:>8}",
        "Model", "Rust", "Py", "TS", "JS", "Go"
    );
    eprintln!("{}", "-".repeat(70));
    for (name, _, _, _, _, _, _, per_lang) in &all_results {
        let mut row = format!("{:<25}", name);
        for (_, lang_mrr, _) in per_lang {
            row += &format!(" {:>7.4}", lang_mrr);
        }
        eprintln!("{}", row);
    }
    eprintln!();
}

// ===== Reranker eval - cross-encoder second pass =====

#[test]
#[ignore] // Slow - downloads models. Run with: cargo test reranker -- --ignored --nocapture
fn test_hard_reranker_comparison() {
    use cqs::parser::ChunkType;
    use cqs::reranker::Reranker;
    use cqs::store::SearchResult;
    use std::path::PathBuf;

    let parser = Parser::new().expect("Failed to initialize parser");
    let e5_config = &MODELS[0]; // E5-base-v2 (production model)

    let languages = [
        Language::Rust,
        Language::Python,
        Language::TypeScript,
        Language::JavaScript,
        Language::Go,
    ];

    // Parse original + hard fixtures
    struct ChunkDesc {
        name: String,
        language: Language,
        nl_text: String,
        content: String,
    }

    let mut chunk_descs: Vec<ChunkDesc> = Vec::new();
    for lang in &languages {
        let path = fixture_path(*lang);
        let chunks = parser
            .parse_file(&path)
            .expect("Failed to parse original fixture");
        for chunk in &chunks {
            let nl = generate_nl_description(chunk);
            chunk_descs.push(ChunkDesc {
                name: chunk.name.clone(),
                language: *lang,
                nl_text: nl,
                content: chunk.content.clone(),
            });
        }
        let hard_path = hard_fixture_path(*lang);
        if hard_path.exists() {
            let chunks = parser
                .parse_file(&hard_path)
                .expect("Failed to parse hard fixture");
            for chunk in &chunks {
                let nl = generate_nl_description(chunk);
                chunk_descs.push(ChunkDesc {
                    name: chunk.name.clone(),
                    language: *lang,
                    nl_text: nl,
                    content: chunk.content.clone(),
                });
            }
        }
    }
    eprintln!(
        "Parsed {} chunks from original + hard fixtures\n",
        chunk_descs.len()
    );

    // Embed with E5-base-v2
    eprintln!("--- E5-base-v2 embedding ---");
    let mut embedder = EvalEmbedder::new(e5_config).expect("Failed to load E5-base-v2");

    eprintln!("  Embedding {} chunks...", chunk_descs.len());
    let nl_texts: Vec<&str> = chunk_descs.iter().map(|c| c.nl_text.as_str()).collect();
    let mut all_embeddings: Vec<Vec<f32>> = Vec::new();
    for batch in nl_texts.chunks(16) {
        let embs = embedder.embed_documents(batch).expect("Embedding failed");
        all_embeddings.extend(embs);
    }
    assert_eq!(
        all_embeddings.len(),
        chunk_descs.len(),
        "Embedding count mismatch"
    );

    let indexed: Vec<IndexedChunk> = chunk_descs
        .iter()
        .zip(all_embeddings.into_iter())
        .map(|(desc, emb)| IndexedChunk {
            name: desc.name.clone(),
            language: desc.language,
            embedding: emb,
        })
        .collect();

    // Pre-embed all queries
    eprintln!("  Embedding {} queries...", HARD_EVAL_CASES.len());
    let query_embeddings: Vec<Vec<f32>> = HARD_EVAL_CASES
        .iter()
        .map(|case| {
            embedder
                .embed_query(case.query)
                .expect("Query embed failed")
        })
        .collect();

    // Load reranker
    eprintln!("\n--- Loading reranker ---");
    let reranker = Reranker::new().expect("Failed to create reranker");

    // For each query: get top-20 by embedding, then rerank
    const CANDIDATE_K: usize = 20;
    let mut emb_reciprocal_ranks: Vec<f64> = Vec::new();
    let mut rerank_reciprocal_ranks: Vec<f64> = Vec::new();
    let mut emb_r1 = 0usize;
    let mut rerank_r1 = 0usize;
    let mut emb_ndcg_sum = 0.0f64;
    let mut rerank_ndcg_sum = 0.0f64;

    eprintln!(
        "\n  Evaluating {} queries (top-{} candidates):\n",
        HARD_EVAL_CASES.len(),
        CANDIDATE_K
    );

    for (case, query_embedding) in HARD_EVAL_CASES.iter().zip(query_embeddings.iter()) {
        // Step 1: Embedding retrieval — top-K candidates by cosine similarity
        let mut scored: Vec<(usize, f32)> = indexed
            .iter()
            .enumerate()
            .filter(|(_, c)| c.language == case.language)
            .map(|(i, c)| (i, cosine_similarity(query_embedding, &c.embedding)))
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        scored.truncate(CANDIDATE_K);

        // Embedding-only rank
        let emb_rank = scored
            .iter()
            .position(|(i, _)| {
                let name = &indexed[*i].name;
                name == case.expected_name || case.also_accept.contains(&name.as_str())
            })
            .map(|pos| pos + 1);

        let emb_rr = emb_rank.map(|r| 1.0 / r as f64).unwrap_or(0.0);
        emb_reciprocal_ranks.push(emb_rr);
        if emb_rank == Some(1) {
            emb_r1 += 1;
        }
        emb_ndcg_sum += emb_rank
            .map(|r| 1.0 / (r as f64 + 1.0).log2())
            .unwrap_or(0.0);

        // Step 2: Rerank candidates with cross-encoder
        // Build SearchResult objects for the reranker API
        let mut search_results: Vec<SearchResult> = scored
            .iter()
            .map(|(i, score)| {
                let desc = &chunk_descs[*i];
                SearchResult {
                    chunk: cqs::store::ChunkSummary {
                        id: format!("eval-{}", i),
                        file: PathBuf::from("eval_fixture"),
                        language: desc.language,
                        chunk_type: ChunkType::Function,
                        name: desc.name.clone(),
                        signature: String::new(),
                        content: desc.content.clone(),
                        doc: None,
                        line_start: 0,
                        line_end: 0,
                        content_hash: String::new(),
                        window_idx: None,
                        parent_id: None,
                        parent_type_name: None,
                    },
                    score: *score,
                }
            })
            .collect();

        // Use NL descriptions as passages for the cross-encoder
        let passage_texts: Vec<&str> = scored
            .iter()
            .map(|(i, _)| chunk_descs[*i].nl_text.as_str())
            .collect();

        reranker
            .rerank_with_passages(case.query, &mut search_results, &passage_texts, CANDIDATE_K)
            .expect("Reranking failed");

        // Reranked rank
        let rerank_rank = search_results
            .iter()
            .position(|r| {
                r.chunk.name == case.expected_name
                    || case.also_accept.contains(&r.chunk.name.as_str())
            })
            .map(|pos| pos + 1);

        let rerank_rr = rerank_rank.map(|r| 1.0 / r as f64).unwrap_or(0.0);
        rerank_reciprocal_ranks.push(rerank_rr);
        if rerank_rank == Some(1) {
            rerank_r1 += 1;
        }
        rerank_ndcg_sum += rerank_rank
            .map(|r| 1.0 / (r as f64 + 1.0).log2())
            .unwrap_or(0.0);

        // Per-query output
        let emb_rank_str = emb_rank
            .map(|r| r.to_string())
            .unwrap_or_else(|| "miss".to_string());
        let rerank_rank_str = rerank_rank
            .map(|r| r.to_string())
            .unwrap_or_else(|| "miss".to_string());
        let delta = match (emb_rank, rerank_rank) {
            (Some(e), Some(r)) if r < e => format!(" (+{})", e - r),
            (Some(e), Some(r)) if r > e => format!(" (-{})", r - e),
            (None, Some(_)) => " (rescued)".to_string(),
            (Some(_), None) => " (lost)".to_string(),
            _ => String::new(),
        };
        eprintln!(
            "  [{:?}] \"{}\" -> exp: {}, emb: {}, rerank: {}{}",
            case.language, case.query, case.expected_name, emb_rank_str, rerank_rank_str, delta
        );
    }

    let n = HARD_EVAL_CASES.len() as f64;
    let emb_mrr = emb_reciprocal_ranks.iter().sum::<f64>() / n;
    let rerank_mrr = rerank_reciprocal_ranks.iter().sum::<f64>() / n;
    let emb_r1_pct = emb_r1 as f64 / n * 100.0;
    let rerank_r1_pct = rerank_r1 as f64 / n * 100.0;
    let emb_ndcg = emb_ndcg_sum / n;
    let rerank_ndcg = rerank_ndcg_sum / n;

    // Comparison table
    eprintln!(
        "\n=== Reranker Impact (top-{} candidates, {} queries) ===\n",
        CANDIDATE_K,
        HARD_EVAL_CASES.len()
    );
    eprintln!(
        "{:<25} {:>10} {:>10} {:>10}",
        "Method", "Recall@1", "MRR", "NDCG@10"
    );
    eprintln!("{}", "-".repeat(58));
    eprintln!(
        "{:<25} {:>9.1}% {:>10.4} {:>10.4}",
        "Embedding only", emb_r1_pct, emb_mrr, emb_ndcg
    );
    eprintln!(
        "{:<25} {:>9.1}% {:>10.4} {:>10.4}",
        "Embedding + Reranker", rerank_r1_pct, rerank_mrr, rerank_ndcg
    );
    eprintln!(
        "{:<25} {:>+9.1}% {:>+10.4} {:>+10.4}",
        "Delta",
        rerank_r1_pct - emb_r1_pct,
        rerank_mrr - emb_mrr,
        rerank_ndcg - emb_ndcg
    );
    eprintln!();
}

/// Quick test to verify ONNX op graph for CUDA compatibility
/// Checks if a model has rotary embedding ops that would cause CPU fallback
#[test]
#[ignore]
fn test_cuda_compatibility() {
    use hf_hub::api::sync::Api;

    eprintln!("=== CUDA Compatibility Check ===\n");
    eprintln!("Checking ONNX op graphs for rotary embedding ops...\n");

    let api = Api::new().expect("Failed to init HF API");

    for model_config in MODELS {
        eprintln!("--- {} ---", model_config.name);

        let repo = api.model(model_config.repo.to_string());
        let model_path = match repo.get(model_config.model_file) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("  SKIP: {}\n", e);
                continue;
            }
        };

        // Load session and inspect
        let session = match Session::builder().and_then(|mut b| b.commit_from_file(&model_path)) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("  SKIP: {}\n", e);
                continue;
            }
        };

        // Report inputs/outputs
        eprintln!("  Inputs:");
        for input in session.inputs().iter() {
            eprintln!("    {} {:?}", input.name(), input.dtype());
        }
        eprintln!("  Outputs:");
        for output in session.outputs().iter() {
            eprintln!("    {} {:?}", output.name(), output.dtype());
        }

        // Check architecture by output dimension
        eprintln!("  Expected output dim: {}", model_config.output_dim);
        eprintln!(
            "  Architecture: {} ({})",
            if model_config.output_dim <= cqs::EMBEDDING_DIM {
                "base"
            } else {
                "large"
            },
            if model_config.needs_token_type_ids {
                "BERT-style, absolute position embeddings"
            } else {
                "check manually for rotary embeddings"
            }
        );
        eprintln!();
    }
}

// ===== Enriched hard eval - with LLM summaries =====

/// Load pre-generated LLM summaries from fixture file
fn load_fixture_summaries() -> HashMap<String, String> {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
    let path = std::path::Path::new(&manifest_dir).join("tests/fixtures/eval_hard_summaries.json");
    let content = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("Failed to read {}: {}", path.display(), e));
    serde_json::from_str(&content)
        .unwrap_or_else(|e| panic!("Failed to parse {}: {}", path.display(), e))
}

/// Generate enriched NL: prepend summary (if available) to base NL description.
/// Matches production behavior in `generate_nl_with_call_context_and_summary`.
fn generate_enriched_nl(chunk: &cqs::parser::Chunk, summary: Option<&str>) -> String {
    let base_nl = generate_nl_description(chunk);
    match summary {
        Some(s) if !s.is_empty() => format!("{} {}", s, base_nl),
        _ => base_nl,
    }
}

#[test]
#[ignore] // Requires model files. Run with: cargo test hard_with_summaries -- --ignored --nocapture
fn test_hard_with_summaries() {
    let parser = Parser::new().expect("Failed to initialize parser");
    let e5_config = &MODELS[0]; // E5-base-v2 (production model)
    let mut embedder = EvalEmbedder::new(e5_config).expect("Failed to load E5-base-v2");

    let summaries = load_fixture_summaries();
    eprintln!("Loaded {} fixture summaries\n", summaries.len());

    let languages = [
        Language::Rust,
        Language::Python,
        Language::TypeScript,
        Language::JavaScript,
        Language::Go,
    ];

    // Parse BOTH original and hard fixtures — combined corpus (same as test_hard_model_comparison)
    let mut chunks: Vec<cqs::parser::Chunk> = Vec::new();
    for lang in &languages {
        let path = fixture_path(*lang);
        let parsed = parser
            .parse_file(&path)
            .expect("Failed to parse original fixture");
        chunks.extend(parsed);
        let hard_path = hard_fixture_path(*lang);
        if hard_path.exists() {
            let parsed = parser
                .parse_file(&hard_path)
                .expect("Failed to parse hard fixture");
            chunks.extend(parsed);
        }
    }

    // Generate enriched NL descriptions (prepend summary where available)
    let mut with_summary = 0usize;
    let mut without_summary = 0usize;

    struct ChunkDesc {
        name: String,
        language: Language,
        nl_text: String,
    }

    let mut chunk_descs: Vec<ChunkDesc> = Vec::new();
    for chunk in &chunks {
        let summary = summaries.get(&chunk.name).map(|s| s.as_str());
        if summary.is_some() {
            with_summary += 1;
        } else {
            without_summary += 1;
        }
        let nl = generate_enriched_nl(chunk, summary);
        chunk_descs.push(ChunkDesc {
            name: chunk.name.clone(),
            language: chunk.language,
            nl_text: nl,
        });
    }

    eprintln!(
        "Parsed {} chunks: {} with summaries, {} without\n",
        chunk_descs.len(),
        with_summary,
        without_summary
    );

    // Show a sample enriched NL
    if let Some(desc) = chunk_descs.iter().find(|d| summaries.contains_key(&d.name)) {
        eprintln!(
            "  Sample enriched: {}\n",
            &desc.nl_text[..desc.nl_text.len().min(150)]
        );
    }

    // Embed all chunk descriptions
    eprintln!("Embedding {} chunks...", chunk_descs.len());
    let nl_texts: Vec<&str> = chunk_descs.iter().map(|c| c.nl_text.as_str()).collect();
    let mut all_embeddings: Vec<Vec<f32>> = Vec::new();
    for batch in nl_texts.chunks(16) {
        let embs = embedder.embed_documents(batch).expect("Embedding failed");
        all_embeddings.extend(embs);
    }
    assert_eq!(
        all_embeddings.len(),
        chunk_descs.len(),
        "Embedding count mismatch"
    );

    let indexed: Vec<IndexedChunk> = chunk_descs
        .iter()
        .zip(all_embeddings.into_iter())
        .map(|(desc, emb)| IndexedChunk {
            name: desc.name.clone(),
            language: desc.language,
            embedding: emb,
        })
        .collect();

    // Pre-embed all queries
    eprintln!("Embedding {} queries...", HARD_EVAL_CASES.len());
    let query_embeddings: Vec<Vec<f32>> = HARD_EVAL_CASES
        .iter()
        .map(|case| {
            embedder
                .embed_query(case.query)
                .expect("Query embed failed")
        })
        .collect();

    // Detailed per-query output
    eprintln!("\n  Hard eval cases ({} queries):", HARD_EVAL_CASES.len());
    for (case, query_embedding) in HARD_EVAL_CASES.iter().zip(query_embeddings.iter()) {
        let mut scored: Vec<(&str, f32)> = indexed
            .iter()
            .filter(|c| c.language == case.language)
            .map(|c| {
                (
                    c.name.as_str(),
                    cosine_similarity(query_embedding, &c.embedding),
                )
            })
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());

        let rank = scored
            .iter()
            .position(|(name, _)| *name == case.expected_name)
            .map(|pos| pos + 1);

        let top5: Vec<&str> = scored.iter().take(5).map(|(n, _)| *n).collect();
        let status = match rank {
            Some(1) => "+",
            Some(r) if r <= 5 => "~",
            _ => "-",
        };
        eprintln!(
            "  {} [{:?}] \"{}\" -> exp: {} (rank: {}), top5: {:?}",
            status,
            case.language,
            case.query,
            case.expected_name,
            rank.map(|r| r.to_string()).unwrap_or("miss".to_string()),
            top5
        );
    }

    // Compute metrics
    let (r1_hits, r1_total) = compute_recall_at_k(&indexed, HARD_EVAL_CASES, &query_embeddings, 1);
    let (r5_hits, r5_total) = compute_recall_at_k(&indexed, HARD_EVAL_CASES, &query_embeddings, 5);
    let (_, per_lang_mrr) = compute_mrr(&indexed, HARD_EVAL_CASES, &query_embeddings);
    let ndcg_10 = compute_ndcg_at_k(&indexed, HARD_EVAL_CASES, &query_embeddings, 10);

    let recall_1 = r1_hits as f64 / r1_total as f64;
    let recall_5 = r5_hits as f64 / r5_total as f64;

    // Print results
    eprintln!("\n=== Enriched Hard Eval (with LLM summaries) ===\n");
    eprintln!(
        "  Coverage: {}/{} chunks have summaries ({:.0}%)",
        with_summary,
        chunk_descs.len(),
        with_summary as f64 / chunk_descs.len() as f64 * 100.0
    );
    eprintln!(
        "  Recall@1: {}/{} ({:.1}%)",
        r1_hits,
        r1_total,
        recall_1 * 100.0
    );
    eprintln!(
        "  Recall@5: {}/{} ({:.1}%)",
        r5_hits,
        r5_total,
        recall_5 * 100.0
    );
    eprintln!("  NDCG@10: {:.4}", ndcg_10);
    eprintln!("\n  Per-language MRR:");
    for (lang, lang_mrr, count) in &per_lang_mrr {
        eprintln!("    {:?}: {:.4} ({} queries)", lang, lang_mrr, count);
    }
    eprintln!();

    // Gate: enriched pipeline should achieve at least 85% R@1
    assert!(
        recall_1 >= 0.85,
        "Enriched hard eval R@1 {:.1}% below 85% threshold",
        recall_1 * 100.0
    );
}

// ===== Unit tests for retrieval metrics (TC-7) =====
// These test the pure metric functions without requiring model downloads.

#[cfg(test)]
mod metric_tests {
    use super::*;

    /// Build a small corpus of 4 "chunks" with orthogonal unit vectors as embeddings.
    /// Queries are exact copies of one chunk's embedding, so cosine similarity = 1.0
    /// for the match and 0.0 for others.
    fn make_indexed() -> Vec<IndexedChunk> {
        vec![
            IndexedChunk {
                name: "alpha".to_string(),
                language: Language::Rust,
                embedding: vec![1.0, 0.0, 0.0, 0.0],
            },
            IndexedChunk {
                name: "beta".to_string(),
                language: Language::Rust,
                embedding: vec![0.0, 1.0, 0.0, 0.0],
            },
            IndexedChunk {
                name: "gamma".to_string(),
                language: Language::Rust,
                embedding: vec![0.0, 0.0, 1.0, 0.0],
            },
            IndexedChunk {
                name: "delta".to_string(),
                language: Language::Rust,
                embedding: vec![0.0, 0.0, 0.0, 1.0],
            },
        ]
    }

    fn make_cases() -> Vec<EvalCase> {
        vec![
            EvalCase {
                query: "find alpha",
                expected_name: "alpha",
                language: Language::Rust,
                also_accept: &[],
            },
            EvalCase {
                query: "find gamma",
                expected_name: "gamma",
                language: Language::Rust,
                also_accept: &[],
            },
        ]
    }

    // --- MRR tests ---

    #[test]
    fn test_mrr_perfect_ranking() {
        let indexed = make_indexed();
        let cases = make_cases();
        // Query embeddings match alpha and gamma exactly → rank 1 for both
        let query_embeddings = vec![
            vec![1.0, 0.0, 0.0, 0.0], // matches alpha
            vec![0.0, 0.0, 1.0, 0.0], // matches gamma
        ];
        let (mrr, _per_lang) = compute_mrr(&indexed, &cases, &query_embeddings);
        assert!(
            (mrr - 1.0).abs() < 1e-6,
            "Perfect match → MRR = 1.0, got {mrr}"
        );
    }

    #[test]
    fn test_mrr_second_rank() {
        let indexed = make_indexed();
        let cases = vec![EvalCase {
            query: "find beta",
            expected_name: "beta",
            language: Language::Rust,
            also_accept: &[],
        }];
        // Query embedding is closer to alpha than beta → beta is rank 2
        let query_embeddings = vec![vec![0.6, 0.5, 0.0, 0.0]];
        let (mrr, _) = compute_mrr(&indexed, &cases, &query_embeddings);
        assert!((mrr - 0.5).abs() < 1e-6, "Rank 2 → RR = 0.5, got {mrr}");
    }

    #[test]
    fn test_mrr_miss() {
        let indexed = make_indexed();
        let cases = vec![EvalCase {
            query: "find missing",
            expected_name: "nonexistent",
            language: Language::Rust,
            also_accept: &[],
        }];
        let query_embeddings = vec![vec![1.0, 0.0, 0.0, 0.0]];
        let (mrr, _) = compute_mrr(&indexed, &cases, &query_embeddings);
        assert!((mrr).abs() < 1e-6, "Missing target → MRR = 0.0, got {mrr}");
    }

    #[test]
    fn test_mrr_empty_cases() {
        let indexed = make_indexed();
        let (mrr, per_lang) = compute_mrr(&indexed, &[], &[]);
        assert!((mrr).abs() < 1e-6, "No cases → MRR = 0.0");
        assert!(per_lang.is_empty());
    }

    // --- Recall@K tests ---

    #[test]
    fn test_recall_at_1_perfect() {
        let indexed = make_indexed();
        let cases = make_cases();
        let query_embeddings = vec![vec![1.0, 0.0, 0.0, 0.0], vec![0.0, 0.0, 1.0, 0.0]];
        let (hits, total) = compute_recall_at_k(&indexed, &cases, &query_embeddings, 1);
        assert_eq!(hits, 2);
        assert_eq!(total, 2);
    }

    #[test]
    fn test_recall_at_1_partial() {
        let indexed = make_indexed();
        let cases = make_cases();
        // First query matches alpha (rank 1), second is closer to delta than gamma (miss at k=1)
        let query_embeddings = vec![
            vec![1.0, 0.0, 0.0, 0.0],
            vec![0.0, 0.0, 0.3, 0.8], // delta > gamma
        ];
        let (hits, total) = compute_recall_at_k(&indexed, &cases, &query_embeddings, 1);
        assert_eq!(hits, 1, "Only alpha found at rank 1");
        assert_eq!(total, 2);
    }

    #[test]
    fn test_recall_at_k_increases_with_k() {
        let indexed = make_indexed();
        let cases = make_cases();
        // gamma query is close to delta (rank 1) then gamma (rank 2)
        let query_embeddings = vec![vec![1.0, 0.0, 0.0, 0.0], vec![0.0, 0.0, 0.3, 0.8]];
        let (h1, _) = compute_recall_at_k(&indexed, &cases, &query_embeddings, 1);
        let (h3, _) = compute_recall_at_k(&indexed, &cases, &query_embeddings, 3);
        assert!(h3 >= h1, "Recall@3 >= Recall@1");
    }

    #[test]
    fn test_recall_empty_cases() {
        let indexed = make_indexed();
        let (hits, total) = compute_recall_at_k(&indexed, &[], &[], 5);
        assert_eq!(hits, 0);
        assert_eq!(total, 0);
    }

    // --- NDCG@K tests ---

    #[test]
    fn test_ndcg_perfect_ranking() {
        let indexed = make_indexed();
        let cases = make_cases();
        let query_embeddings = vec![vec![1.0, 0.0, 0.0, 0.0], vec![0.0, 0.0, 1.0, 0.0]];
        let ndcg = compute_ndcg_at_k(&indexed, &cases, &query_embeddings, 10);
        // Both at rank 1 → NDCG = 1/log2(2) = 1.0
        assert!(
            (ndcg - 1.0).abs() < 1e-6,
            "Perfect → NDCG = 1.0, got {ndcg}"
        );
    }

    #[test]
    fn test_ndcg_lower_rank() {
        let indexed = make_indexed();
        let cases = vec![EvalCase {
            query: "find beta",
            expected_name: "beta",
            language: Language::Rust,
            also_accept: &[],
        }];
        // beta at rank 2 → NDCG = 1/log2(3) ≈ 0.631
        let query_embeddings = vec![vec![0.6, 0.5, 0.0, 0.0]];
        let ndcg = compute_ndcg_at_k(&indexed, &cases, &query_embeddings, 10);
        let expected = 1.0 / 3.0_f64.log2();
        assert!(
            (ndcg - expected).abs() < 1e-4,
            "Rank 2 → NDCG ≈ {expected:.4}, got {ndcg:.4}"
        );
    }

    #[test]
    fn test_ndcg_miss() {
        let indexed = make_indexed();
        let cases = vec![EvalCase {
            query: "find missing",
            expected_name: "nonexistent",
            language: Language::Rust,
            also_accept: &[],
        }];
        let query_embeddings = vec![vec![1.0, 0.0, 0.0, 0.0]];
        let ndcg = compute_ndcg_at_k(&indexed, &cases, &query_embeddings, 10);
        assert!((ndcg).abs() < 1e-6, "Miss → NDCG = 0.0, got {ndcg}");
    }

    #[test]
    fn test_ndcg_empty_cases() {
        let indexed = make_indexed();
        let ndcg = compute_ndcg_at_k(&indexed, &[], &[], 10);
        assert!((ndcg).abs() < 1e-6, "No cases → NDCG = 0.0");
    }

    // --- cosine_similarity unit tests ---

    #[test]
    fn test_cosine_identity() {
        let v = vec![1.0, 0.0, 0.0];
        assert!((cosine_similarity(&v, &v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_orthogonal() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        assert!((cosine_similarity(&a, &b)).abs() < 1e-6);
    }
}

// ===== Experiment 7: Type-Aware Embeddings =====
//
// Test whether prepending function signatures to NL descriptions
// improves discrimination between confusable functions.

#[test]
#[ignore] // Slow: downloads model, embeds corpus
fn test_type_aware_embeddings() {
    let parser = Parser::new().expect("Failed to init parser");
    let e5_config = &MODELS[0];
    let languages = [
        Language::Rust,
        Language::Python,
        Language::TypeScript,
        Language::JavaScript,
        Language::Go,
    ];

    struct TypeAwareChunk {
        name: String,
        language: Language,
        nl_base: String,
        nl_sig_prepend: String,
        nl_sig_append: String,
        signature: String,
    }

    let mut chunks: Vec<TypeAwareChunk> = Vec::new();
    for lang in &languages {
        for path in [fixture_path(*lang), hard_fixture_path(*lang)] {
            if !path.exists() {
                continue;
            }
            let parsed = parser.parse_file(&path).expect("Parse failed");
            for chunk in &parsed {
                let base_nl = generate_nl_description(chunk);
                let sig = &chunk.signature;

                // Variant 1: prepend signature
                let sig_prepend = if sig.is_empty() {
                    base_nl.clone()
                } else {
                    format!("{}. {}", sig, base_nl)
                };

                // Variant 2: append signature
                let sig_append = if sig.is_empty() {
                    base_nl.clone()
                } else {
                    format!("{}. Signature: {}", base_nl, sig)
                };

                chunks.push(TypeAwareChunk {
                    name: chunk.name.clone(),
                    language: *lang,
                    nl_base: base_nl,
                    nl_sig_prepend: sig_prepend,
                    nl_sig_append: sig_append,
                    signature: sig.clone(),
                });
            }
        }
    }
    eprintln!("Parsed {} chunks\n", chunks.len());

    // Show sample NL descriptions for comparison
    if let Some(c) = chunks
        .iter()
        .find(|c| c.name == "merge_sort" && c.language == Language::Rust)
    {
        eprintln!("Sample (merge_sort Rust):");
        eprintln!("  Base:    {}", &c.nl_base[..c.nl_base.len().min(150)]);
        eprintln!(
            "  Prepend: {}",
            &c.nl_sig_prepend[..c.nl_sig_prepend.len().min(150)]
        );
        eprintln!("  Sig:     {}", c.signature);
        eprintln!();
    }

    let mut embedder = EvalEmbedder::new(e5_config).expect("Failed to load E5");

    // Embed all three variants
    let configs: Vec<(&str, Box<dyn Fn(&TypeAwareChunk) -> &str>)> = vec![
        (
            "Base (no signature)",
            Box::new(|c: &TypeAwareChunk| c.nl_base.as_str()),
        ),
        (
            "Signature prepended",
            Box::new(|c: &TypeAwareChunk| c.nl_sig_prepend.as_str()),
        ),
        (
            "Signature appended",
            Box::new(|c: &TypeAwareChunk| c.nl_sig_append.as_str()),
        ),
    ];

    // Pre-embed queries (same across all configs)
    let query_embs: Vec<Vec<f32>> = HARD_EVAL_CASES
        .iter()
        .map(|c| embedder.embed_query(c.query).expect("Query embed failed"))
        .collect();

    eprintln!(
        "=== Type-Aware Embedding Results ({} queries) ===\n",
        HARD_EVAL_CASES.len()
    );
    eprintln!(
        "{:<25} {:>10} {:>10} {:>10}",
        "Config", "Recall@1", "MRR", "NDCG@10"
    );
    eprintln!("{}", "-".repeat(58));

    for (label, nl_fn) in &configs {
        let texts: Vec<&str> = chunks.iter().map(|c| nl_fn(c)).collect();
        let mut embs: Vec<Vec<f32>> = Vec::new();
        for batch in texts.chunks(16) {
            embs.extend(embedder.embed_documents(batch).expect("Embed failed"));
        }

        let indexed: Vec<IndexedChunk> = chunks
            .iter()
            .zip(embs.into_iter())
            .map(|(c, e)| IndexedChunk {
                name: c.name.clone(),
                language: c.language,
                embedding: e,
            })
            .collect();

        let (r1_hits, r1_total) = compute_recall_at_k(&indexed, HARD_EVAL_CASES, &query_embs, 1);
        let (mrr, per_lang_mrr) = compute_mrr(&indexed, HARD_EVAL_CASES, &query_embs);
        let ndcg = compute_ndcg_at_k(&indexed, HARD_EVAL_CASES, &query_embs, 10);
        let r1_pct = r1_hits as f64 / r1_total as f64 * 100.0;

        eprintln!(
            "{:<25} {:>9.1}% {:>10.4} {:>10.4}",
            label, r1_pct, mrr, ndcg
        );

        // Per-language detail
        for (lang, lang_mrr, count) in &per_lang_mrr {
            eprintln!("  {:?}: MRR {:.4} ({} queries)", lang, lang_mrr, count);
        }
        eprintln!();
    }
}

// ===== Experiment 6: Weighted Multi-Signal Fusion Sweep =====

struct SweepChunk {
    name: String,
    language: Language,
    content: String,
    nl_text: String,
}

struct ScoringResult {
    label: String,
    r1: f64,
    mrr: f64,
    ndcg10: f64,
}

/// Simple name matching mirroring the production NameMatcher tiers.
fn name_match_score(query: &str, chunk_name: &str) -> f32 {
    let q = query.to_lowercase();
    let n = chunk_name.to_lowercase();
    let q_words: Vec<String> = split_name_words(&q);
    let n_words: Vec<String> = split_name_words(&n);

    if q == n {
        return 1.0;
    }
    if n.contains(&q) {
        return 0.8;
    }
    if q.contains(&n) {
        return 0.6;
    }
    if !q_words.is_empty() && !n_words.is_empty() {
        let overlap = q_words.iter().filter(|w| n_words.contains(w)).count();
        if overlap > 0 {
            let total = q_words.len().max(n_words.len());
            return (overlap as f32 / total as f32) * 0.5;
        }
    }
    0.0
}

fn split_name_words(name: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    for ch in name.chars() {
        if ch == '_' || ch == '-' {
            if !current.is_empty() {
                words.push(current.clone());
                current.clear();
            }
        } else if ch.is_uppercase() && !current.is_empty() {
            words.push(current.clone());
            current.clear();
            current.push(ch.to_lowercase().next().unwrap_or(ch));
        } else {
            current.push(ch.to_lowercase().next().unwrap_or(ch));
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

fn keyword_score(query: &str, chunk_name: &str, chunk_content: &str) -> f32 {
    let query_terms: Vec<&str> = query.split_whitespace().collect();
    if query_terms.is_empty() {
        return 0.0;
    }
    let combined = format!("{} {}", chunk_name, chunk_content).to_lowercase();
    let hits: usize = query_terms
        .iter()
        .filter(|term| combined.contains(&term.to_lowercase()))
        .count();
    hits as f32 / query_terms.len() as f32
}

fn rrf_merge(
    semantic_ranking: &[(usize, f32)],
    keyword_ranking: &[(usize, f32)],
    k: f32,
    semantic_weight: f32,
    keyword_weight: f32,
) -> Vec<(usize, f32)> {
    let mut scores: HashMap<usize, f32> = HashMap::new();
    for (rank, &(idx, _)) in semantic_ranking.iter().enumerate() {
        *scores.entry(idx).or_insert(0.0) += semantic_weight / (k + rank as f32 + 1.0);
    }
    for (rank, &(idx, _)) in keyword_ranking.iter().enumerate() {
        *scores.entry(idx).or_insert(0.0) += keyword_weight / (k + rank as f32 + 1.0);
    }
    let mut sorted: Vec<(usize, f32)> = scores.into_iter().collect();
    sorted.sort_by(|a, b| b.1.total_cmp(&a.1));
    sorted
}

fn weighted_score(emb: f32, nm: f32, kw: f32, name_boost: f32, kw_boost: f32) -> f32 {
    (1.0 - name_boost - kw_boost) * emb + name_boost * nm + kw_boost * kw
}

#[test]
#[ignore] // Slow: downloads model, embeds corpus
fn test_weight_sweep() {
    let parser = Parser::new().expect("Failed to init parser");
    let e5_config = &MODELS[0];
    let languages = [
        Language::Rust,
        Language::Python,
        Language::TypeScript,
        Language::JavaScript,
        Language::Go,
    ];

    let mut chunks: Vec<SweepChunk> = Vec::new();
    for lang in &languages {
        for path in [fixture_path(*lang), hard_fixture_path(*lang)] {
            if !path.exists() {
                continue;
            }
            let parsed = parser.parse_file(&path).expect("Failed to parse fixture");
            for chunk in &parsed {
                chunks.push(SweepChunk {
                    name: chunk.name.clone(),
                    language: *lang,
                    content: chunk.content.clone(),
                    nl_text: generate_nl_description(chunk),
                });
            }
        }
    }
    eprintln!("Parsed {} chunks\n", chunks.len());

    let mut embedder = EvalEmbedder::new(e5_config).expect("Failed to load E5");
    let nl_refs: Vec<&str> = chunks.iter().map(|c| c.nl_text.as_str()).collect();
    let mut all_embs: Vec<Vec<f32>> = Vec::new();
    for batch in nl_refs.chunks(16) {
        all_embs.extend(embedder.embed_documents(batch).expect("Embed failed"));
    }
    let query_embs: Vec<Vec<f32>> = HARD_EVAL_CASES
        .iter()
        .map(|c| embedder.embed_query(c.query).expect("Query embed failed"))
        .collect();
    eprintln!(
        "Embedded {} chunks + {} queries\n",
        all_embs.len(),
        query_embs.len()
    );

    let mut results: Vec<ScoringResult> = Vec::new();

    results.push(eval_sweep(
        "Embedding only",
        &chunks,
        &all_embs,
        &query_embs,
        0.0,
        0.0,
        false,
        60.0,
        1.0,
        1.0,
    ));
    for nb in [0.05, 0.10, 0.15, 0.20, 0.25, 0.30, 0.40] {
        results.push(eval_sweep(
            &format!("name_boost={nb:.2}"),
            &chunks,
            &all_embs,
            &query_embs,
            nb,
            0.0,
            false,
            60.0,
            1.0,
            1.0,
        ));
    }
    for kb in [0.05, 0.10, 0.15, 0.20] {
        results.push(eval_sweep(
            &format!("kw_boost={kb:.2}"),
            &chunks,
            &all_embs,
            &query_embs,
            0.0,
            kb,
            false,
            60.0,
            1.0,
            1.0,
        ));
    }
    for nb in [0.05, 0.10, 0.15] {
        for kb in [0.05, 0.10, 0.15] {
            results.push(eval_sweep(
                &format!("nb={nb:.2}+kb={kb:.2}"),
                &chunks,
                &all_embs,
                &query_embs,
                nb,
                kb,
                false,
                60.0,
                1.0,
                1.0,
            ));
        }
    }
    for k in [20.0, 40.0, 60.0, 80.0] {
        results.push(eval_sweep(
            &format!("RRF k={k:.0} (1:1)"),
            &chunks,
            &all_embs,
            &query_embs,
            0.0,
            0.0,
            true,
            k,
            1.0,
            1.0,
        ));
    }
    for (sw, kw) in [(2.0, 1.0), (3.0, 1.0), (1.0, 2.0)] {
        results.push(eval_sweep(
            &format!("RRF k=60 ({sw}:{kw})"),
            &chunks,
            &all_embs,
            &query_embs,
            0.0,
            0.0,
            true,
            60.0,
            sw,
            kw,
        ));
    }
    for nb in [0.10, 0.20] {
        results.push(eval_sweep(
            &format!("RRF k=60 + nb={nb:.2}"),
            &chunks,
            &all_embs,
            &query_embs,
            nb,
            0.0,
            true,
            60.0,
            1.0,
            1.0,
        ));
    }

    eprintln!(
        "\n=== Weight Sweep Results ({} queries, {} configs) ===\n",
        HARD_EVAL_CASES.len(),
        results.len()
    );
    eprintln!(
        "{:<30} {:>10} {:>10} {:>10} {:>8}",
        "Config", "Recall@1", "MRR", "NDCG@10", "Delta"
    );
    eprintln!("{}", "-".repeat(72));

    let baseline_mrr = results[0].mrr;
    for r in &results {
        let delta = r.mrr - baseline_mrr;
        let marker = if delta > 0.001 {
            " +"
        } else if delta < -0.001 {
            " -"
        } else {
            "  "
        };
        eprintln!(
            "{:<30} {:>9.1}% {:>10.4} {:>10.4} {:>+7.4}{}",
            r.label,
            r.r1 * 100.0,
            r.mrr,
            r.ndcg10,
            delta,
            marker
        );
    }

    let best = results
        .iter()
        .max_by(|a, b| a.mrr.total_cmp(&b.mrr))
        .unwrap();
    eprintln!(
        "\nBest: {} (MRR={:.4}, R@1={:.1}%)",
        best.label,
        best.mrr,
        best.r1 * 100.0
    );
}

#[allow(clippy::too_many_arguments)]
fn eval_sweep(
    label: &str,
    chunks: &[SweepChunk],
    all_embs: &[Vec<f32>],
    query_embs: &[Vec<f32>],
    name_boost: f32,
    kw_boost: f32,
    use_rrf: bool,
    rrf_k: f32,
    semantic_weight: f32,
    keyword_weight: f32,
) -> ScoringResult {
    let mut r1 = 0usize;
    let mut rr_sum = 0.0f64;
    let mut ndcg_sum = 0.0f64;
    let n = HARD_EVAL_CASES.len();

    for (case, q_emb) in HARD_EVAL_CASES.iter().zip(query_embs.iter()) {
        let candidates: Vec<(usize, f32, f32, f32)> = chunks
            .iter()
            .enumerate()
            .filter(|(_, c)| c.language == case.language)
            .map(|(i, c)| {
                let emb = cosine_similarity(q_emb, &all_embs[i]);
                let nm = name_match_score(case.query, &c.name);
                let kw = keyword_score(case.query, &c.name, &c.content);
                (i, emb, nm, kw)
            })
            .collect();

        let ranked: Vec<(usize, f32)> = if use_rrf {
            let mut sem: Vec<(usize, f32)> = candidates
                .iter()
                .map(|&(i, emb, nm, _)| {
                    (
                        i,
                        if name_boost > 0.0 {
                            (1.0 - name_boost) * emb + name_boost * nm
                        } else {
                            emb
                        },
                    )
                })
                .collect();
            sem.sort_by(|a, b| b.1.total_cmp(&a.1));
            let mut kw: Vec<(usize, f32)> =
                candidates.iter().map(|&(i, _, _, kw)| (i, kw)).collect();
            kw.sort_by(|a, b| b.1.total_cmp(&a.1));
            rrf_merge(&sem, &kw, rrf_k, semantic_weight, keyword_weight)
        } else {
            let mut scored: Vec<(usize, f32)> = candidates
                .iter()
                .map(|&(i, emb, nm, kw)| (i, weighted_score(emb, nm, kw, name_boost, kw_boost)))
                .collect();
            scored.sort_by(|a, b| b.1.total_cmp(&a.1));
            scored
        };

        let rank = ranked
            .iter()
            .position(|(i, _)| {
                chunks[*i].name == case.expected_name
                    || case.also_accept.contains(&chunks[*i].name.as_str())
            })
            .map(|p| p + 1);

        if rank == Some(1) {
            r1 += 1;
        }
        rr_sum += rank.map(|r| 1.0 / r as f64).unwrap_or(0.0);
        ndcg_sum += rank.map(|r| 1.0 / (r as f64 + 1.0).log2()).unwrap_or(0.0);
    }

    ScoringResult {
        label: label.to_string(),
        r1: r1 as f64 / n as f64,
        mrr: rr_sum / n as f64,
        ndcg10: ndcg_sum / n as f64,
    }
}
