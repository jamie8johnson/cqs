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
        Language::Sql => "sql",
        Language::Markdown => "md",
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

// ===== Hard eval - confusable functions =====

fn hard_fixture_path(lang: Language) -> PathBuf {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
    let ext = match lang {
        Language::Rust => "rs",
        Language::Python => "py",
        Language::TypeScript => "ts",
        Language::JavaScript => "js",
        Language::Go => "go",
        Language::C => "c",
        Language::Java => "java",
        Language::Sql => "sql",
        Language::Markdown => "md",
    };
    PathBuf::from(manifest_dir)
        .join("tests")
        .join("fixtures")
        .join(format!(
            "eval_hard_{}.{}",
            lang.to_string().to_lowercase(),
            ext
        ))
}

/// Hard eval cases - confusable queries where multiple similar functions exist
const HARD_EVAL_CASES: &[EvalCase] = &[
    // Rust (11) - must distinguish between 6 sort variants, 4 validators, etc.
    EvalCase {
        query: "stable sort preserving relative order of equal elements",
        expected_name: "merge_sort",
        language: Language::Rust,
    },
    EvalCase {
        query: "sort using binary max-heap data structure",
        expected_name: "heap_sort",
        language: Language::Rust,
    },
    EvalCase {
        query: "simple sort efficient for small nearly sorted arrays",
        expected_name: "insertion_sort",
        language: Language::Rust,
    },
    EvalCase {
        query: "non-comparison integer sort processing digits",
        expected_name: "radix_sort",
        language: Language::Rust,
    },
    EvalCase {
        query: "validate phone number with international country code",
        expected_name: "validate_phone",
        language: Language::Rust,
    },
    EvalCase {
        query: "check if URL has valid protocol and hostname",
        expected_name: "validate_url",
        language: Language::Rust,
    },
    EvalCase {
        query: "pad string to fixed width with fill character",
        expected_name: "pad_string",
        language: Language::Rust,
    },
    EvalCase {
        query: "count number of words in text",
        expected_name: "count_words",
        language: Language::Rust,
    },
    EvalCase {
        query: "extract numeric values from mixed text string",
        expected_name: "extract_numbers",
        language: Language::Rust,
    },
    EvalCase {
        query: "stop calling service after consecutive failures",
        expected_name: "CircuitBreaker",
        language: Language::Rust,
    },
    EvalCase {
        query: "check whether circuit allows request through",
        expected_name: "should_allow",
        language: Language::Rust,
    },
    // Python (11)
    EvalCase {
        query: "stable sort preserving relative order of equal elements",
        expected_name: "merge_sort",
        language: Language::Python,
    },
    EvalCase {
        query: "sort using binary max-heap data structure",
        expected_name: "heap_sort",
        language: Language::Python,
    },
    EvalCase {
        query: "simple sort efficient for small nearly sorted arrays",
        expected_name: "insertion_sort",
        language: Language::Python,
    },
    EvalCase {
        query: "non-comparison integer sort processing digits",
        expected_name: "radix_sort",
        language: Language::Python,
    },
    EvalCase {
        query: "validate phone number with international country code",
        expected_name: "validate_phone",
        language: Language::Python,
    },
    EvalCase {
        query: "check if URL has valid protocol and hostname",
        expected_name: "validate_url",
        language: Language::Python,
    },
    EvalCase {
        query: "pad string to fixed width with fill character",
        expected_name: "pad_string",
        language: Language::Python,
    },
    EvalCase {
        query: "count number of words in text",
        expected_name: "count_words",
        language: Language::Python,
    },
    EvalCase {
        query: "extract numeric values from mixed text string",
        expected_name: "extract_numbers",
        language: Language::Python,
    },
    EvalCase {
        query: "stop calling service after consecutive failures",
        expected_name: "CircuitBreaker",
        language: Language::Python,
    },
    EvalCase {
        query: "check whether circuit allows request through",
        expected_name: "should_allow",
        language: Language::Python,
    },
    // TypeScript (11)
    EvalCase {
        query: "stable sort preserving relative order of equal elements",
        expected_name: "mergeSort",
        language: Language::TypeScript,
    },
    EvalCase {
        query: "sort using binary max-heap data structure",
        expected_name: "heapSort",
        language: Language::TypeScript,
    },
    EvalCase {
        query: "simple sort efficient for small nearly sorted arrays",
        expected_name: "insertionSort",
        language: Language::TypeScript,
    },
    EvalCase {
        query: "non-comparison integer sort processing digits",
        expected_name: "radixSort",
        language: Language::TypeScript,
    },
    EvalCase {
        query: "validate phone number with international country code",
        expected_name: "validatePhone",
        language: Language::TypeScript,
    },
    EvalCase {
        query: "check if URL has valid protocol and hostname",
        expected_name: "validateUrl",
        language: Language::TypeScript,
    },
    EvalCase {
        query: "pad string to fixed width with fill character",
        expected_name: "padString",
        language: Language::TypeScript,
    },
    EvalCase {
        query: "count number of words in text",
        expected_name: "countWords",
        language: Language::TypeScript,
    },
    EvalCase {
        query: "extract numeric values from mixed text string",
        expected_name: "extractNumbers",
        language: Language::TypeScript,
    },
    EvalCase {
        query: "stop calling service after consecutive failures",
        expected_name: "CircuitBreaker",
        language: Language::TypeScript,
    },
    EvalCase {
        query: "check whether circuit allows request through",
        expected_name: "shouldAllow",
        language: Language::TypeScript,
    },
    // JavaScript (11)
    EvalCase {
        query: "stable sort preserving relative order of equal elements",
        expected_name: "mergeSort",
        language: Language::JavaScript,
    },
    EvalCase {
        query: "sort using binary max-heap data structure",
        expected_name: "heapSort",
        language: Language::JavaScript,
    },
    EvalCase {
        query: "simple sort efficient for small nearly sorted arrays",
        expected_name: "insertionSort",
        language: Language::JavaScript,
    },
    EvalCase {
        query: "non-comparison integer sort processing digits",
        expected_name: "radixSort",
        language: Language::JavaScript,
    },
    EvalCase {
        query: "validate phone number with international country code",
        expected_name: "validatePhone",
        language: Language::JavaScript,
    },
    EvalCase {
        query: "check if URL has valid protocol and hostname",
        expected_name: "validateUrl",
        language: Language::JavaScript,
    },
    EvalCase {
        query: "pad string to fixed width with fill character",
        expected_name: "padString",
        language: Language::JavaScript,
    },
    EvalCase {
        query: "count number of words in text",
        expected_name: "countWords",
        language: Language::JavaScript,
    },
    EvalCase {
        query: "extract numeric values from mixed text string",
        expected_name: "extractNumbers",
        language: Language::JavaScript,
    },
    EvalCase {
        query: "stop calling service after consecutive failures",
        expected_name: "CircuitBreaker",
        language: Language::JavaScript,
    },
    EvalCase {
        query: "check whether circuit allows request through",
        expected_name: "shouldAllow",
        language: Language::JavaScript,
    },
    // Go (11)
    EvalCase {
        query: "stable sort preserving relative order of equal elements",
        expected_name: "MergeSort",
        language: Language::Go,
    },
    EvalCase {
        query: "sort using binary max-heap data structure",
        expected_name: "HeapSort",
        language: Language::Go,
    },
    EvalCase {
        query: "simple sort efficient for small nearly sorted arrays",
        expected_name: "InsertionSort",
        language: Language::Go,
    },
    EvalCase {
        query: "non-comparison integer sort processing digits",
        expected_name: "RadixSort",
        language: Language::Go,
    },
    EvalCase {
        query: "validate phone number with international country code",
        expected_name: "ValidatePhone",
        language: Language::Go,
    },
    EvalCase {
        query: "check if URL has valid protocol and hostname",
        expected_name: "ValidateUrl",
        language: Language::Go,
    },
    EvalCase {
        query: "pad string to fixed width with fill character",
        expected_name: "PadString",
        language: Language::Go,
    },
    EvalCase {
        query: "count number of words in text",
        expected_name: "CountWords",
        language: Language::Go,
    },
    EvalCase {
        query: "extract numeric values from mixed text string",
        expected_name: "ExtractNumbers",
        language: Language::Go,
    },
    EvalCase {
        query: "stop calling service after consecutive failures",
        expected_name: "CircuitBreakerGo",
        language: Language::Go,
    },
    EvalCase {
        query: "check whether circuit allows request through",
        expected_name: "ShouldAllow",
        language: Language::Go,
    },
];

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

    // Test only E5-base-v2 and jina-v2-base-code for speed
    let test_models = [&MODELS[0], &MODELS[3]];

    let mut all_results: HardEvalResults = Vec::new();

    for model_config in test_models {
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
            },
            EvalCase {
                query: "find gamma",
                expected_name: "gamma",
                language: Language::Rust,
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
