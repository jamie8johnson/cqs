//! Model evaluation harness - compare embedding models for code search quality
//!
//! Run with: cargo test model_eval -- --ignored --nocapture
//!
//! This evaluates alternative embedding models against the same 50-query eval suite
//! used for production. Models are compared on raw Recall@5 without Store/FTS/name-boost
//! to isolate embedding quality.
//!
//! CUDA gate: only BERT-style models (absolute position embeddings) are candidates.
//! Models with rotary embeddings (nomic, Qwen3) cause ort CPU fallback thrashing.

use cqs::parser::{Language, Parser};
use cqs::{generate_nl_description, generate_nl_with_template, NlTemplate};
use ndarray::Array2;
use ort::session::Session;
use ort::value::Tensor;
use std::collections::HashMap;
use std::path::PathBuf;

/// Per-model evaluation results: (name, per-language hits/total, total hits, total queries)
type EvalResults<'a> = Vec<(&'a str, HashMap<Language, (usize, usize)>, usize, usize)>;

// ===== Model Configuration =====

struct ModelConfig {
    name: &'static str,
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
];

// ===== Eval Cases (same as eval_test.rs) =====

struct EvalCase {
    query: &'static str,
    expected_name: &'static str,
    language: Language,
}

const EVAL_CASES: &[EvalCase] = &[
    // Rust (10)
    EvalCase {
        query: "retry with exponential backoff",
        expected_name: "retry_with_backoff",
        language: Language::Rust,
    },
    EvalCase {
        query: "validate email address format",
        expected_name: "validate_email",
        language: Language::Rust,
    },
    EvalCase {
        query: "parse JSON configuration file",
        expected_name: "parse_json_config",
        language: Language::Rust,
    },
    EvalCase {
        query: "compute SHA256 hash",
        expected_name: "hash_sha256",
        language: Language::Rust,
    },
    EvalCase {
        query: "format number as currency with commas",
        expected_name: "format_currency",
        language: Language::Rust,
    },
    EvalCase {
        query: "convert camelCase to snake_case",
        expected_name: "camel_to_snake",
        language: Language::Rust,
    },
    EvalCase {
        query: "truncate string with ellipsis",
        expected_name: "truncate_string",
        language: Language::Rust,
    },
    EvalCase {
        query: "check if string is valid UUID",
        expected_name: "is_valid_uuid",
        language: Language::Rust,
    },
    EvalCase {
        query: "sort array with quicksort algorithm",
        expected_name: "quicksort",
        language: Language::Rust,
    },
    EvalCase {
        query: "memoize function results",
        expected_name: "get_or_compute",
        language: Language::Rust,
    },
    // Python (10)
    EvalCase {
        query: "retry with exponential backoff",
        expected_name: "retry_with_backoff",
        language: Language::Python,
    },
    EvalCase {
        query: "validate email address format",
        expected_name: "validate_email",
        language: Language::Python,
    },
    EvalCase {
        query: "parse JSON config from file",
        expected_name: "parse_json_config",
        language: Language::Python,
    },
    EvalCase {
        query: "compute SHA256 hash of bytes",
        expected_name: "hash_sha256",
        language: Language::Python,
    },
    EvalCase {
        query: "format currency with dollar sign",
        expected_name: "format_currency",
        language: Language::Python,
    },
    EvalCase {
        query: "convert camelCase to snake_case",
        expected_name: "camel_to_snake",
        language: Language::Python,
    },
    EvalCase {
        query: "truncate string with ellipsis",
        expected_name: "truncate_string",
        language: Language::Python,
    },
    EvalCase {
        query: "check UUID format validity",
        expected_name: "is_valid_uuid",
        language: Language::Python,
    },
    EvalCase {
        query: "quicksort sorting algorithm",
        expected_name: "quicksort",
        language: Language::Python,
    },
    EvalCase {
        query: "cache function results decorator",
        expected_name: "memoize",
        language: Language::Python,
    },
    // TypeScript (10)
    EvalCase {
        query: "retry operation with exponential backoff",
        expected_name: "retryWithBackoff",
        language: Language::TypeScript,
    },
    EvalCase {
        query: "validate email address",
        expected_name: "validateEmail",
        language: Language::TypeScript,
    },
    EvalCase {
        query: "parse JSON config string",
        expected_name: "parseJsonConfig",
        language: Language::TypeScript,
    },
    EvalCase {
        query: "SHA256 hash computation",
        expected_name: "hashSha256",
        language: Language::TypeScript,
    },
    EvalCase {
        query: "format money with commas",
        expected_name: "formatCurrency",
        language: Language::TypeScript,
    },
    EvalCase {
        query: "camelCase to snake_case conversion",
        expected_name: "camelToSnake",
        language: Language::TypeScript,
    },
    EvalCase {
        query: "truncate long string with dots",
        expected_name: "truncateString",
        language: Language::TypeScript,
    },
    EvalCase {
        query: "UUID format validation",
        expected_name: "isValidUuid",
        language: Language::TypeScript,
    },
    EvalCase {
        query: "quicksort implementation",
        expected_name: "quicksort",
        language: Language::TypeScript,
    },
    EvalCase {
        query: "memoization cache wrapper",
        expected_name: "memoize",
        language: Language::TypeScript,
    },
    // JavaScript (10)
    EvalCase {
        query: "retry with exponential backoff delay",
        expected_name: "retryWithBackoff",
        language: Language::JavaScript,
    },
    EvalCase {
        query: "email validation regex",
        expected_name: "validateEmail",
        language: Language::JavaScript,
    },
    EvalCase {
        query: "JSON configuration parser",
        expected_name: "parseJsonConfig",
        language: Language::JavaScript,
    },
    EvalCase {
        query: "SHA256 cryptographic hash",
        expected_name: "hashSha256",
        language: Language::JavaScript,
    },
    EvalCase {
        query: "currency formatter",
        expected_name: "formatCurrency",
        language: Language::JavaScript,
    },
    EvalCase {
        query: "convert camel case to snake case",
        expected_name: "camelToSnake",
        language: Language::JavaScript,
    },
    EvalCase {
        query: "string truncation with ellipsis",
        expected_name: "truncateString",
        language: Language::JavaScript,
    },
    EvalCase {
        query: "UUID validation check",
        expected_name: "isValidUuid",
        language: Language::JavaScript,
    },
    EvalCase {
        query: "quicksort divide and conquer",
        expected_name: "quicksort",
        language: Language::JavaScript,
    },
    EvalCase {
        query: "function result memoization",
        expected_name: "memoize",
        language: Language::JavaScript,
    },
    // Go (10)
    EvalCase {
        query: "retry with exponential backoff",
        expected_name: "RetryWithBackoff",
        language: Language::Go,
    },
    EvalCase {
        query: "email address validation",
        expected_name: "ValidateEmail",
        language: Language::Go,
    },
    EvalCase {
        query: "parse JSON config file",
        expected_name: "ParseJsonConfig",
        language: Language::Go,
    },
    EvalCase {
        query: "compute SHA256 hash",
        expected_name: "HashSha256",
        language: Language::Go,
    },
    EvalCase {
        query: "format currency with commas",
        expected_name: "FormatCurrency",
        language: Language::Go,
    },
    EvalCase {
        query: "camelCase to snake_case",
        expected_name: "CamelToSnake",
        language: Language::Go,
    },
    EvalCase {
        query: "truncate string ellipsis",
        expected_name: "TruncateString",
        language: Language::Go,
    },
    EvalCase {
        query: "validate UUID format",
        expected_name: "IsValidUuid",
        language: Language::Go,
    },
    EvalCase {
        query: "quicksort algorithm",
        expected_name: "Quicksort",
        language: Language::Go,
    },
    EvalCase {
        query: "memoization get or compute",
        expected_name: "GetOrCompute",
        language: Language::Go,
    },
];

// ===== Eval Embedder (model-agnostic) =====

struct EvalEmbedder {
    session: Session,
    tokenizer: tokenizers::Tokenizer,
    config: &'static ModelConfig,
}

impl EvalEmbedder {
    fn new(config: &'static ModelConfig) -> Result<Self, Box<dyn std::error::Error>> {
        use hf_hub::api::sync::Api;

        eprintln!("  Downloading {} from {}...", config.name, config.repo);
        let api = Api::new()?;
        let repo = api.model(config.repo.to_string());

        let model_path = repo.get(config.model_file)?;
        let tokenizer_path = repo.get(config.tokenizer_file)?;

        eprintln!("  Creating ONNX session (CPU)...");
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

fn fixture_path(lang: Language) -> PathBuf {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
    let ext = match lang {
        Language::Rust => "rs",
        Language::Python => "py",
        Language::TypeScript => "ts",
        Language::JavaScript => "js",
        Language::Go => "go",
        Language::C => "c",
        Language::Java => "java",
    };
    PathBuf::from(manifest_dir)
        .join("tests")
        .join("fixtures")
        .join(format!("eval_{}.{}", lang.to_string().to_lowercase(), ext))
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
        ("Standard (baseline)", NlTemplate::Standard),
        ("NoPrefix", NlTemplate::NoPrefix),
        ("BodyKeywords", NlTemplate::BodyKeywords),
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
        let session = match Session::builder().and_then(|b| b.commit_from_file(&model_path)) {
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
            if model_config.output_dim <= 768 {
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
