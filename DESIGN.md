# cq - Code Query

**Version:** 0.10.0-draft
**Updated:** 2026-01-31T06:00:00Z

Semantic search over local codebases using embeddings. Find patterns, not files.

---

## Problem

Claude Code sees files it's told to see. Doesn't know project patterns. Reinvents instead of reuses.

## Solution

Embed code chunks, store locally, query semantically. "How does this project handle X?" returns actual examples from your code.

## Design Principles

1. **Zero setup** - `cargo install cq && cq init && cq index` works
2. **Local only** - No accounts, no API keys, no data leaving machine
3. **Fast** - GPU when available, CPU always works
4. **Explicit** - No magic updates, user controls versioning
5. **Multi-language** - Same tool, any codebase

## Non-Goals

- Not a replacement for grep/ripgrep (use those for exact matches)
- Not cross-project search (one index per project)
- Not a code intelligence server (no go-to-definition, no LSP)
- Not macro-aware (sees invocation, not expansion)
- Not multi-hop (single-chunk retrieval, user synthesizes)

---

## Architecture

```
┌──────────────┐     ┌──────────────┐     ┌──────────────┐
│   Parser     │────▶│   Embedder   │────▶│    Store     │
│(tree-sitter) │     │ (ort + hf    │     │   (sqlite)   │
│              │     │  tokenizers) │     │              │
└──────────────┘     └──────────────┘     └──────────────┘
       │                   │
       │            ┌──────┴──────┐
       │            │  Runtime    │
       │            │  Detection  │
       │            └─────────────┘
       │            CUDA → TensorRT → CPU
       │
  ┌────┴────┐
  │ Grammar │
  │  Store  │
  └─────────┘
  rust, python, typescript, go...
```

**Why tree-sitter instead of syn?**

| Factor | syn | tree-sitter |
|--------|-----|-------------|
| Languages | Rust only | 100+ languages |
| Error recovery | Fails on invalid syntax | Parses broken/partial code |
| Comments | Manual extraction | First-class nodes |
| Ecosystem | Proc-macro focused | Editor/analysis focused |
| FFI | Pure Rust | C library + Rust bindings |

syn is excellent for proc-macros. tree-sitter is built for exactly what we're doing: parsing code for analysis. The C FFI is battle-tested (Helix, Zed, Neovim, GitHub).

**Why ort + tokenizers instead of fastembed-rs?**

fastembed-rs doesn't expose execution provider configuration. For GPU support, we need direct control over ort session creation.

---

## Model Details

**Model:** nomic-embed-text-v1.5  
**Source:** https://huggingface.co/nomic-ai/nomic-embed-text-v1.5  
**License:** Apache 2.0  
**Dimensions:** 768 (supports Matryoshka truncation to 512, 256)  
**Context:** 8192 tokens  
**Parameters:** 137M  

**ONNX file:**
- `onnx/model.onnx` - float32, ~547MB
- `onnx/model_fp16.onnx` - float16, ~274MB (recommended: 2x smaller, minimal quality loss)

**Critical: Task prefixes required**

nomic-embed-text requires task-specific prefixes for optimal performance:

```
# When indexing code chunks (documents)
"search_document: fn retry_with_backoff<F, T>..."

# When querying
"search_query: retry with exponential backoff"
```

Without prefixes, retrieval quality degrades significantly. Non-negotiable.

---

## Components

### 1. Parser (tree-sitter)

**Input:** Source files (any supported language)  
**Output:** Chunks with metadata

Uses tree-sitter to parse source into concrete syntax trees (CST), then extracts semantic units.

```rust
pub struct Parser {
    languages: HashMap<String, tree_sitter::Language>,
}

impl Parser {
    pub fn new() -> Result<Self>;
    pub fn parse_file(&self, path: &Path) -> Result<Vec<Chunk>>;
    pub fn supported_extensions(&self) -> &[&str];
}

pub struct Chunk {
    id: String,              // {relative_path}:{line_start}:{content_hash[..8]}
    file: PathBuf,           // relative to project root
    language: Language,
    chunk_type: ChunkType,
    name: String,
    signature: String,       // function/method signature
    content: String,         // full source text of the node
    doc: Option<String>,     // doc comments (/// or /** */)
    line_start: u32,
    line_end: u32,
    content_hash: String,    // blake3
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Rust,
    Python,
    TypeScript,
    JavaScript,
    Go,
    // Phase 2: C, Cpp, Java, Ruby, etc.
}

impl Language {
    pub fn from_extension(ext: &str) -> Option<Self>;
    pub fn grammar(&self) -> tree_sitter::Language;
    pub fn comment_patterns(&self) -> &[&str];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkType {
    Function,
    Method,
    // Phase 2: Class, Struct, Enum, Trait, Interface, Module
}

impl std::fmt::Display for Language {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Language::Rust => write!(f, "rust"),
            Language::Python => write!(f, "python"),
            Language::TypeScript => write!(f, "typescript"),
            Language::JavaScript => write!(f, "javascript"),
            Language::Go => write!(f, "go"),
        }
    }
}

impl std::str::FromStr for Language {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "rust" => Ok(Language::Rust),
            "python" => Ok(Language::Python),
            "typescript" => Ok(Language::TypeScript),
            "javascript" => Ok(Language::JavaScript),
            "go" => Ok(Language::Go),
            _ => bail!("Unknown language: {}", s),
        }
    }
}

impl std::fmt::Display for ChunkType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChunkType::Function => write!(f, "function"),
            ChunkType::Method => write!(f, "method"),
        }
    }
}

impl std::str::FromStr for ChunkType {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "function" => Ok(ChunkType::Function),
            "method" => Ok(ChunkType::Method),
            _ => bail!("Unknown chunk type: {}", s),
        }
    }
}
```

**Language detection:**

```rust
impl Language {
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext {
            "rs" => Some(Language::Rust),
            "py" | "pyi" => Some(Language::Python),
            "ts" | "tsx" => Some(Language::TypeScript),
            "js" | "jsx" | "mjs" | "cjs" => Some(Language::JavaScript),
            "go" => Some(Language::Go),
            _ => None,
        }
    }
}
```

**tree-sitter query patterns (per language):**

Each language needs a query to extract functions/methods. tree-sitter queries use S-expression patterns.

```rust
impl Language {
    pub fn query_pattern(&self) -> &'static str {
        match self {
            Language::Rust => RUST_QUERY,
            Language::Python => PYTHON_QUERY,
            Language::TypeScript | Language::JavaScript => TYPESCRIPT_QUERY,
            Language::Go => GO_QUERY,
        }
    }
}

// Rust: capture all function_item nodes
const RUST_QUERY: &str = r#"
(function_item
  name: (identifier) @name) @function
"#;

// Python: capture all function_definition nodes
const PYTHON_QUERY: &str = r#"
(function_definition
  name: (identifier) @name) @function
"#;

// TypeScript/JavaScript: functions, methods, arrow functions
// Note: Standalone arrow_function pattern may duplicate assigned arrows.
// Deduplicate by byte range in extract_chunk() or filter in post-processing.
const TYPESCRIPT_QUERY: &str = r#"
(function_declaration
  name: (identifier) @name) @function

(method_definition
  name: (property_identifier) @name) @function

;; Arrow function assigned to variable: const foo = () => {}
(lexical_declaration
  (variable_declarator
    name: (identifier) @name
    value: (arrow_function) @function))

;; Arrow function assigned with var/let
(variable_declaration
  (variable_declarator
    name: (identifier) @name
    value: (arrow_function) @function))
"#;
// Removed standalone (arrow_function) @function to avoid duplicates.
// Anonymous callbacks in .map()/.filter() won't be indexed - acceptable for MVP.

// Go: functions and methods
const GO_QUERY: &str = r#"
(function_declaration
  name: (identifier) @name) @function

(method_declaration
  name: (field_identifier) @name) @function
"#;
```

**Note on queries:**
- `@function` captures the entire node (we extract content from this)
- `@name` captures just the identifier (for display, optional for anonymous functions)
- Use `query.capture_index_for_name()` to find captures by name, not by node kind
- For Go, `infer_chunk_type()` checks node type directly; for others, checks parent chain

**Chunk extraction:**

```rust
use tree_sitter::StreamingIterator;  // Required for query iteration

impl Language {
    pub fn grammar(&self) -> tree_sitter::Language {
        match self {
            Language::Rust => tree_sitter_rust::LANGUAGE.into(),
            Language::Python => tree_sitter_python::LANGUAGE.into(),
            Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Language::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Language::Go => tree_sitter_go::LANGUAGE.into(),
        }
    }
}

impl Parser {
    pub fn parse_file(&self, path: &Path) -> Result<Vec<Chunk>> {
        let source = std::fs::read_to_string(path)?;
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        
        let language = Language::from_extension(ext)
            .ok_or_else(|| anyhow!("Unsupported file type: {}", ext))?;
        
        let grammar = language.grammar();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&grammar)
            .map_err(|e| anyhow!("Failed to set language: {:?}", e))?;
        
        let tree = parser.parse(&source, None)
            .ok_or_else(|| anyhow!("Failed to parse {}", path.display()))?;
        
        let query = tree_sitter::Query::new(&grammar, language.query_pattern())
            .map_err(|e| anyhow!("Invalid query: {:?}", e))?;
        let mut cursor = tree_sitter::QueryCursor::new();
        
        // Note: matches() returns StreamingIterator, requires trait in scope
        let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());
        
        let mut chunks = Vec::new();
        
        while let Some(m) = matches.next() {
            let chunk = self.extract_chunk(&source, m, &query, language, path)?;
            
            // Skip chunks over 100 lines
            let lines = chunk.line_end - chunk.line_start;
            if lines > 100 {
                tracing::warn!(
                    "Skipping {} ({} lines > 100 max)",
                    chunk.id, lines
                );
                continue;
            }
            
            chunks.push(chunk);
        }
        
        Ok(chunks)
    }
    
    fn extract_chunk(
        &self,
        source: &str,
        m: &tree_sitter::QueryMatch<'_, '_>,
        query: &tree_sitter::Query,
        language: Language,
        path: &Path,
    ) -> Result<Chunk> {
        // Get capture indices by name from query
        let func_idx = query.capture_index_for_name("function")
            .ok_or_else(|| anyhow!("Query missing @function capture"))?;
        let name_idx = query.capture_index_for_name("name");  // Optional for arrow functions
        
        // Find the function node by capture index
        let func_capture = m.captures.iter()
            .find(|c| c.index == func_idx)
            .ok_or_else(|| anyhow!("Missing @function capture in match"))?;
        let node = func_capture.node;
        
        // Find name by capture index (may not exist for anonymous functions)
        let name = name_idx
            .and_then(|idx| m.captures.iter().find(|c| c.index == idx))
            .map(|c| source[c.node.byte_range()].to_string())
            .unwrap_or_else(|| "<anonymous>".to_string());
        
        // Extract content
        let content = source[node.byte_range()].to_string();
        
        // Line numbers (1-indexed for display)
        let line_start = node.start_position().row + 1;
        let line_end = node.end_position().row + 1;
        
        // Extract signature (first line for most languages)
        let signature = self.extract_signature(&content, language);
        
        // Extract doc comments (look at preceding siblings)
        let doc = self.extract_doc_comment(node, source, language);
        
        // Determine chunk type from the captured node's parent context
        let chunk_type = self.infer_chunk_type(node, language);
        
        let content_hash = blake3::hash(content.as_bytes()).to_hex().to_string();
        let id = format!("{}:{}:{}", path.display(), line_start, &content_hash[..8]);
        
        Ok(Chunk {
            id,
            file: path.to_path_buf(),
            language,
            chunk_type,
            name,
            signature,
            content,
            doc,
            line_start: line_start as u32,
            line_end: line_end as u32,
            content_hash,
        })
    }
    
    fn infer_chunk_type(&self, node: tree_sitter::Node, language: Language) -> ChunkType {
        // For Go, the node type itself determines function vs method
        // method_declaration = has receiver, function_declaration = no receiver
        if language == Language::Go {
            return if node.kind() == "method_declaration" {
                ChunkType::Method
            } else {
                ChunkType::Function
            };
        }
        
        // For other languages, check if function is inside a class/impl/struct body
        let mut current = node.parent();
        while let Some(parent) = current {
            let kind = parent.kind();
            let is_method_container = match language {
                Language::Rust => kind == "impl_item" || kind == "trait_item",
                Language::Python => kind == "class_definition",
                Language::TypeScript | Language::JavaScript => {
                    kind == "class_body" || kind == "class_declaration"
                }
                Language::Go => unreachable!(),  // Handled above
            };
            if is_method_container {
                return ChunkType::Method;
            }
            current = parent.parent();
        }
        ChunkType::Function
    }
    
    fn extract_signature(&self, content: &str, language: Language) -> String {
        // Extract up to first { or : (language dependent)
        let sig_end = match language {
            Language::Rust | Language::Go | Language::TypeScript | Language::JavaScript => {
                content.find('{').unwrap_or(content.len())
            }
            Language::Python => {
                content.find(':').unwrap_or(content.len())
            }
        };
        
        let sig = &content[..sig_end];
        // Normalize whitespace
        sig.split_whitespace().collect::<Vec<_>>().join(" ")
    }
    
    fn extract_doc_comment(
        &self,
        node: tree_sitter::Node,
        source: &str,
        language: Language,
    ) -> Option<String> {
        // Walk backwards through siblings looking for comments
        let mut comments = Vec::new();
        let mut current = node.prev_sibling();
        
        while let Some(sibling) = current {
            let kind = sibling.kind();
            
            let is_doc = match language {
                Language::Rust => kind == "line_comment" || kind == "block_comment",
                Language::Python => kind == "string" || kind == "comment",  // docstrings
                Language::TypeScript | Language::JavaScript => kind == "comment",
                Language::Go => kind == "comment",
            };
            
            if is_doc {
                let text = &source[sibling.byte_range()];
                comments.push(text.to_string());
                current = sibling.prev_sibling();
            } else if sibling.kind().contains("comment") {
                // Keep looking
                current = sibling.prev_sibling();
            } else {
                break;
            }
        }
        
        if comments.is_empty() {
            // For Python, also check for docstring as first statement in body
            if language == Language::Python {
                if let Some(body) = node.child_by_field_name("body") {
                    // Use named_child to skip whitespace/comments
                    if let Some(first) = body.named_child(0) {
                        if first.kind() == "expression_statement" {
                            if let Some(string) = first.named_child(0) {
                                if string.kind() == "string" {
                                    return Some(source[string.byte_range()].to_string());
                                }
                            }
                        }
                    }
                }
            }
            return None;
        }
        
        comments.reverse();
        Some(comments.join("\n"))
    }
}
```

**Project root detection:**
- Walk up from CWD looking for:
  - `Cargo.toml` (Rust)
  - `package.json` (JS/TS)
  - `pyproject.toml` or `setup.py` (Python)
  - `go.mod` (Go)
  - `.git`
- If found, that's project root
- If not found, use CWD but warn

**Path handling:**
- Store paths relative to `.cq/` parent directory
- Normalize to forward slashes on all platforms
- On index: resolve relative to project root
- On query: display as stored

**File enumeration (safety):**

```rust
use walkdir::WalkDir;

const MAX_FILE_SIZE: u64 = 1_048_576;  // 1MB

fn enumerate_files(root: &Path) -> Result<Vec<PathBuf>> {
    let root = root.canonicalize()?;  // Resolve symlinks in root path

    let files: Vec<PathBuf> = WalkDir::new(&root)
        .follow_links(false)  // Don't follow symlinks (security)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| {
            // Skip files over size limit
            e.metadata().map(|m| m.len() <= MAX_FILE_SIZE).unwrap_or(false)
        })
        .filter(|e| {
            // Only supported extensions
            e.path().extension()
                .and_then(|ext| ext.to_str())
                .and_then(Language::from_extension)
                .is_some()
        })
        .map(|e| {
            // Validate path stays within project root
            let path = e.path().canonicalize().ok()?;
            if path.starts_with(&root) {
                Some(path)
            } else {
                tracing::warn!("Skipping path outside project: {}", e.path().display());
                None
            }
        })
        .flatten()
        .collect();

    Ok(files)
}
```

**Safety guarantees:**
- Symlinks not followed (prevents escape attacks)
- Files >1MB skipped (prevents OOM)
- Paths validated to stay within project root
- Non-UTF8 files handled gracefully (see parse_file)

**UTF-8 handling in parse_file:**

```rust
pub fn parse_file(&self, path: &Path) -> Result<Vec<Chunk>> {
    // Gracefully handle non-UTF8 files
    let source = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
            tracing::warn!("Skipping non-UTF8 file: {}", path.display());
            return Ok(vec![]);  // Return empty, don't abort
        }
        Err(e) => return Err(e.into()),
    };
    // ... rest of parsing
}
```

### 2. Embedder

**ort for inference, tokenizers for encoding. Runtime GPU detection.**

```rust
pub struct Embedder {
    session: ort::Session,
    tokenizer: tokenizers::Tokenizer,
    provider: ExecutionProvider,
    max_length: usize,   // 8192 for nomic
    batch_size: usize,   // 16 (GPU) or 4 (CPU)
}

#[derive(Debug, Clone, Copy)]
pub enum ExecutionProvider {
    CUDA { device_id: i32 },
    TensorRT { device_id: i32 },
    CPU,
}

impl Embedder {
    pub fn new() -> Result<Self>;
    pub fn embed_documents(&self, texts: &[&str]) -> Result<Vec<Embedding>>;
    pub fn embed_query(&self, text: &str) -> Result<Embedding>;
    pub fn provider(&self) -> ExecutionProvider;
    pub fn warm(&self) -> Result<()>;  // dummy inference to load model
}

pub struct Embedding(pub Vec<f32>);  // 768 dimensions
```

**Execution provider selection:**

```rust
use ort::execution_providers as ep;

fn select_provider() -> ExecutionProvider {
    // Try in order, first available wins
    // Note: is_available() returns Result<bool>

    if ep::CUDA::default().is_available().unwrap_or(false) {
        return ExecutionProvider::CUDA { device_id: 0 };
    }

    if ep::TensorRT::default().is_available().unwrap_or(false) {
        return ExecutionProvider::TensorRT { device_id: 0 };
    }

    ExecutionProvider::CPU
}
```

**Session creation:**

```rust
use ort::Session;
use ort::execution_providers as ep;
use ndarray::Array2;

fn create_session(model_path: &Path, provider: ExecutionProvider) -> Result<Session> {
    let builder = Session::builder()?;

    match provider {
        ExecutionProvider::CUDA { device_id } => {
            builder.with_execution_providers([
                ep::CUDA::default()
                    .with_device_id(device_id)
                    .build()
            ])?
        }
        ExecutionProvider::TensorRT { device_id } => {
            builder.with_execution_providers([
                ep::TensorRT::default()
                    .with_device_id(device_id)
                    .build(),
                // Fallback to CUDA for unsupported ops
                ep::CUDA::default()
                    .with_device_id(device_id)
                    .build()
            ])?
        }
        ExecutionProvider::CPU => builder,
    }
    .commit_from_file(model_path)
}
```

**Model + tokenizer download (using hf-hub crate):**

```rust
use hf_hub::api::sync::Api;

const MODEL_REPO: &str = "nomic-ai/nomic-embed-text-v1.5";
const MODEL_FILE: &str = "onnx/model.onnx";  // ~547MB float32 (or model_fp16.onnx ~274MB)
const TOKENIZER_FILE: &str = "tokenizer.json";

// SHA256 checksums for model verification (update when model changes)
// Get from: sha256sum ~/.cache/huggingface/hub/models--nomic-ai--nomic-embed-text-v1.5/...
const MODEL_SHA256: &str = ""; // TODO: Fill after first download
const TOKENIZER_SHA256: &str = ""; // TODO: Fill after first download

fn ensure_model() -> Result<(PathBuf, PathBuf)> {
    // hf-hub handles caching automatically (~/.cache/huggingface/hub/)
    let api = Api::new()?;
    let repo = api.model(MODEL_REPO.to_string());

    let model_path = repo.get(MODEL_FILE)?;
    let tokenizer_path = repo.get(TOKENIZER_FILE)?;

    // Verify checksums (skip if not configured)
    if !MODEL_SHA256.is_empty() {
        verify_checksum(&model_path, MODEL_SHA256)?;
    }
    if !TOKENIZER_SHA256.is_empty() {
        verify_checksum(&tokenizer_path, TOKENIZER_SHA256)?;
    }

    Ok((model_path, tokenizer_path))
}

fn verify_checksum(path: &Path, expected: &str) -> Result<()> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    std::io::copy(&mut file, &mut hasher)?;
    let actual = hasher.finalize().to_hex().to_string();

    if actual != expected {
        bail!(
            "Checksum mismatch for {}: expected {}, got {}",
            path.display(), expected, actual
        );
    }
    Ok(())
}
```

**ONNX tensor interface:**

```rust
// Inputs:
//   - input_ids: INT32, shape [batch, seq_len]
//   - attention_mask: INT32, shape [batch, seq_len]
//   - token_type_ids: NOT REQUIRED (nomic model doesn't use it)
//
// Outputs:
//   - token_embeddings: FP32, shape [batch, seq_len, 768] - per-token
//   - sentence_embedding: FP32, shape [batch, 768] - PRE-POOLED, use this
```

**Embedding with task prefixes:**

```rust
impl Embedder {
    /// Embed documents (code chunks). Adds "search_document: " prefix.
    pub fn embed_documents(&self, texts: &[&str]) -> Result<Vec<Embedding>> {
        let prefixed: Vec<String> = texts
            .iter()
            .map(|t| format!("search_document: {}", t))
            .collect();
        self.embed_batch(&prefixed)
    }
    
    /// Embed a query. Adds "search_query: " prefix.
    /// Returns error if query is empty or whitespace-only.
    pub fn embed_query(&self, text: &str) -> Result<Embedding> {
        let text = text.trim();
        if text.is_empty() {
            bail!("Query cannot be empty");
        }
        let prefixed = format!("search_query: {}", text);
        let results = self.embed_batch(&[prefixed])?;
        Ok(results.into_iter().next().unwrap())
    }
    
    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Embedding>> {
        // Tokenize
        let encodings = self.tokenizer.encode_batch(texts.to_vec(), true)
            .map_err(|e| anyhow!("Tokenization failed: {}", e))?;
        
        // Prepare inputs - note INT32 not i64
        let input_ids: Vec<Vec<i32>> = encodings
            .iter()
            .map(|e| e.get_ids().iter().map(|&id| id as i32).collect())
            .collect();
        let attention_mask: Vec<Vec<i32>> = encodings
            .iter()
            .map(|e| e.get_attention_mask().iter().map(|&m| m as i32).collect())
            .collect();
        
        // Pad to max length in batch
        let max_len = input_ids.iter().map(|v| v.len()).max().unwrap_or(0);
        
        // Create padded arrays
        let input_ids_arr = pad_2d_i32(&input_ids, max_len, 0);
        let attention_mask_arr = pad_2d_i32(&attention_mask, max_len, 0);
        
        // Run inference - ort 2.x inputs! macro accepts arrays directly
        let outputs = self.session.run(ort::inputs![
            "input_ids" => input_ids_arr,
            "attention_mask" => attention_mask_arr,
        ])?;
        
        // Use sentence_embedding directly - it's pre-pooled
        // ort 2.x: try_extract_array returns ArrayViewD<f32>
        let embeddings: ndarray::ArrayViewD<f32> = outputs["sentence_embedding"]
            .try_extract_array()?;
        
        // L2 normalize - use axis_iter for dynamic-dimension arrays
        Ok(embeddings
            .axis_iter(ndarray::Axis(0))
            .map(|row| {
                let v: Vec<f32> = row.iter().copied().collect();
                Embedding(normalize_l2(v))
            })
            .collect())
    }
}

fn pad_2d_i32(inputs: &[Vec<i32>], max_len: usize, pad_value: i32) -> Array2<i32> {
    let batch_size = inputs.len();
    let mut arr = Array2::from_elem((batch_size, max_len), pad_value);
    for (i, seq) in inputs.iter().enumerate() {
        for (j, &val) in seq.iter().enumerate() {
            arr[[i, j]] = val;
        }
    }
    arr
}

fn normalize_l2(v: Vec<f32>) -> Vec<f32> {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm == 0.0 { v } else { v.into_iter().map(|x| x / norm).collect() }
}
```

**Similarity (optimized for normalized vectors):**

```rust
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    // Both vectors are L2-normalized in normalize_l2(), so cosine = dot product
    // We always normalize on embed, so this is safe
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}
```

**Embedding serialization (for BLOB storage):**

```rust
fn embedding_to_bytes(embedding: &Embedding) -> Vec<u8> {
    embedding.0.iter()
        .flat_map(|f| f.to_le_bytes())
        .collect()
}

fn bytes_to_embedding(bytes: &[u8]) -> Vec<f32> {
    bytes.chunks_exact(4)
        .map(|chunk| f32::from_le_bytes(chunk.try_into().unwrap()))
        .collect()
}
```

**Note:** All embeddings are L2-normalized in `normalize_l2()` before storage. This is non-negotiable - cosine similarity requires it.

**Batching:**
- Batch size: 16 (GPU), 4 (CPU)
- On failure: retry individual items
- Log failures, continue, report summary

**Token limit handling:**
- Max 8192 tokens per input
- Estimate: chars / 4
- Truncate content if over, warn in logs

### 3. Store

**sqlite with WAL mode, BLOB embeddings, brute-force search**

```rust
use std::collections::HashMap;
use rusqlite::Connection;

pub struct Store {
    conn: Connection,
}
```

```sql
PRAGMA journal_mode=WAL;
PRAGMA busy_timeout=5000;

CREATE TABLE metadata (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE chunks (
    id TEXT PRIMARY KEY,
    file TEXT NOT NULL,
    language TEXT NOT NULL,
    chunk_type TEXT NOT NULL,
    name TEXT NOT NULL,
    signature TEXT NOT NULL,
    content TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    doc TEXT,
    line_start INTEGER NOT NULL,
    line_end INTEGER NOT NULL,
    embedding BLOB NOT NULL,
    file_mtime INTEGER NOT NULL,  -- Unix timestamp when file was indexed
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX idx_chunks_file ON chunks(file);
CREATE INDEX idx_chunks_content_hash ON chunks(content_hash);
CREATE INDEX idx_chunks_name ON chunks(name);
CREATE INDEX idx_chunks_language ON chunks(language);
```

**Metadata keys:**
- `schema_version` - for migrations (start at 1)
- `model_name` - "nomic-embed-text-v1.5"
- `dimensions` - 768
- `created_at` - index creation time
- `updated_at` - last modification
- `cq_version` - cq version that created index

**Connection setup with version checking:**

```rust
use fs2::FileExt;

impl Store {
    fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;

        // Enable WAL mode for better concurrent read performance
        conn.pragma_update(None, "journal_mode", "WAL")?;
        // Wait up to 5s if database is locked
        conn.pragma_update(None, "busy_timeout", 5000)?;
        // NORMAL sync is safe with WAL and faster than FULL
        conn.pragma_update(None, "synchronous", "NORMAL")?;

        let store = Self { conn };

        // Check schema version compatibility
        store.check_schema_version()?;
        // Check model version compatibility
        store.check_model_version()?;

        Ok(store)
    }

    fn check_schema_version(&self) -> Result<()> {
        // metadata.value is TEXT, parse to i32
        let version: i32 = self.conn
            .query_row(
                "SELECT value FROM metadata WHERE key = 'schema_version'",
                [],
                |r| r.get::<_, String>(0)
            )
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        if version > CURRENT_SCHEMA_VERSION {
            bail!("Index created by newer cq version (schema {}). Please upgrade cq.", version);
        }
        if version < CURRENT_SCHEMA_VERSION {
            // Run migrations or prompt user
            bail!("Index schema outdated (v{}). Run 'cq index --force' to rebuild.", version);
        }
        Ok(())
    }

    fn check_model_version(&self) -> Result<()> {
        let stored_model: String = self.conn
            .query_row("SELECT value FROM metadata WHERE key = 'model_name'", [], |r| r.get(0))
            .unwrap_or_default();

        if !stored_model.is_empty() && stored_model != MODEL_NAME {
            bail!(
                "Index uses different model '{}'. Current model is '{}'. Run 'cq index --force' to re-embed.",
                stored_model, MODEL_NAME
            );
        }
        Ok(())
    }
}

const CURRENT_SCHEMA_VERSION: i32 = 1;
const MODEL_NAME: &str = "nomic-embed-text-v1.5";
```

**Index lock (prevent concurrent indexing):**

```rust
fn acquire_index_lock(cq_dir: &Path) -> Result<std::fs::File> {
    let lock_path = cq_dir.join("index.lock");
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .open(&lock_path)?;

    lock_file.try_lock_exclusive().map_err(|_| {
        anyhow!("Another cq process is indexing. Wait or remove .cq/index.lock")
    })?;

    Ok(lock_file)  // Lock released when file is dropped
}
```
```

**Operations:**

```rust
impl Store {
    fn open(path: &Path) -> Result<Self>;  // See above for implementation
    fn init(&self, model_info: &ModelInfo) -> Result<()>;
    fn upsert_chunk(&self, chunk: &Chunk, embedding: &Embedding) -> Result<()>;
    fn delete_by_file(&self, file: &Path) -> Result<u32>;
    fn prune_missing(&self, existing_files: &HashSet<PathBuf>) -> Result<u32>;
    fn get_by_content_hash(&self, hash: &str) -> Option<Embedding>;
    fn search(&self, query: &Embedding, limit: usize, threshold: f32) -> Result<Vec<SearchResult>>;
    fn search_filtered(&self, query: &Embedding, filter: &SearchFilter, limit: usize) -> Result<Vec<SearchResult>>;
    fn stats(&self) -> Result<IndexStats>;
}

struct SearchFilter {
    languages: Option<Vec<Language>>,
    path_pattern: Option<String>,  // glob
}

struct SearchResult {
    chunk: ChunkSummary,
    score: f32,
}

/// Lightweight chunk info for search results (no embedding, no full content)
struct ChunkSummary {
    id: String,
    file: PathBuf,
    language: Language,
    chunk_type: ChunkType,
    name: String,
    signature: String,
    content: String,  // Full content for display
    doc: Option<String>,
    line_start: u32,
    line_end: u32,
}

/// Raw row from chunks table (for internal use)
struct ChunkRow {
    id: String,
    file: String,
    language: String,
    chunk_type: String,
    name: String,
    signature: String,
    content: String,
    doc: Option<String>,
    line_start: u32,
    line_end: u32,
    embedding: Vec<u8>,
}

impl From<ChunkRow> for ChunkSummary {
    fn from(row: ChunkRow) -> Self {
        ChunkSummary {
            id: row.id,
            file: PathBuf::from(row.file),
            language: row.language.parse().unwrap_or(Language::Rust),
            chunk_type: row.chunk_type.parse().unwrap_or(ChunkType::Function),
            name: row.name,
            signature: row.signature,
            content: row.content,
            doc: row.doc,
            line_start: row.line_start,
            line_end: row.line_end,
        }
    }
}

/// Model metadata for index initialization
struct ModelInfo {
    name: String,       // "nomic-embed-text-v1.5"
    dimensions: u32,    // 768
    version: String,    // Model version/commit
}

/// Index statistics returned by stats()
struct IndexStats {
    total_chunks: u64,
    total_files: u64,
    chunks_by_language: HashMap<Language, u64>,
    chunks_by_type: HashMap<ChunkType, u64>,
    index_size_bytes: u64,
    created_at: String,
    updated_at: String,
    model_name: String,
    schema_version: i32,
}
```

**Embedding storage:**
- Store as BLOB (3072 bytes for 768 f32s)
- Little-endian byte order
- Compress in Phase 2 if needed

**Batch inserts (10x faster):**

```rust
fn upsert_chunks_batch(&mut self, chunks: &[(Chunk, Embedding)], file_mtime: i64) -> Result<usize> {
    let tx = self.conn.transaction()?;

    {
        let mut stmt = tx.prepare_cached(
            "INSERT OR REPLACE INTO chunks (id, file, language, chunk_type, name, signature, content, content_hash, doc, line_start, line_end, embedding, file_mtime, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)"
        )?;

        let now = chrono::Utc::now().to_rfc3339();
        for (chunk, embedding) in chunks {
            stmt.execute(params![
                chunk.id,
                chunk.file.to_string_lossy(),
                chunk.language.to_string(),
                chunk.chunk_type.to_string(),
                chunk.name,
                chunk.signature,
                chunk.content,
                chunk.content_hash,
                chunk.doc,
                chunk.line_start,
                chunk.line_end,
                embedding_to_bytes(embedding),
                file_mtime,
                &now,
                &now,
            ])?;
        }
    }

    tx.commit()?;
    Ok(chunks.len())
}
```

**mtime caching (skip unchanged files):**

```rust
// file_mtime column is in chunks table schema above

fn needs_reindex(&self, path: &Path) -> Result<bool> {
    let current_mtime = path.metadata()?.modified()?
        .duration_since(std::time::UNIX_EPOCH)?.as_secs() as i64;

    // query_row returns Result<T>, .ok() gives Option<T>
    let stored_mtime: Option<i64> = self.conn
        .query_row(
            "SELECT file_mtime FROM chunks WHERE file = ?1 LIMIT 1",
            [path.to_string_lossy()],
            |r| r.get(0)
        ).ok();

    match stored_mtime {
        Some(mtime) if mtime >= current_mtime => Ok(false),  // Skip
        _ => Ok(true),  // Needs reindex
    }
}
```

**Search (brute force):**

```rust
fn search(&self, query: &Embedding, limit: usize, threshold: f32) -> Result<Vec<SearchResult>> {
    let mut stmt = self.conn.prepare(
        "SELECT id, file, language, chunk_type, name, signature, content, doc, line_start, line_end, embedding FROM chunks"
    )?;

    // Extract all data from rows in the closure (can't return Row reference)
    let rows: Vec<_> = stmt.query_map([], |row| {
        Ok(ChunkRow {
            id: row.get(0)?,
            file: row.get(1)?,
            language: row.get(2)?,
            chunk_type: row.get(3)?,
            name: row.get(4)?,
            signature: row.get(5)?,
            content: row.get(6)?,
            doc: row.get(7)?,
            line_start: row.get(8)?,
            line_end: row.get(9)?,
            embedding: row.get(10)?,
        })
    })?.filter_map(|r| r.ok()).collect();

    // Score and filter in Rust
    let mut results: Vec<SearchResult> = rows
        .into_iter()
        .filter_map(|row| {
            let embedding = bytes_to_embedding(&row.embedding);
            let score = cosine_similarity(&query.0, &embedding);
            if score >= threshold {
                Some(SearchResult {
                    chunk: ChunkSummary::from(row),
                    score,
                })
            } else {
                None
            }
        })
        .collect();

    results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
    results.truncate(limit);
    Ok(results)
}
```

### 4. CLI

```
cq - semantic code search

USAGE:
    cq [OPTIONS] <QUERY>
    cq <COMMAND>

COMMANDS:
    init          Download model and create .cq/
    doctor        Check model, index, hardware
    index         Index current project
    stats         Show index statistics
    config        Show/edit configuration
    update-model  Download latest model
    serve         Start MCP server (for Claude Code integration)
    help          Show help

QUERY OPTIONS:
    -n, --limit <N>       Max results (default: 5)
    -t, --threshold <F>   Min similarity 0.0-1.0 (default: 0.3)
    -l, --lang <LANG>     Filter by language (rust, python, typescript, javascript, go)
    -p, --path <GLOB>     Filter by path pattern
    --json                Output as JSON (see JSON Schema below)
    --no-content          Show only file:line, no code

INDEX OPTIONS:
    --force               Re-index all files, ignore mtime cache
    --dry-run             Show what would be indexed, don't write

GLOBAL OPTIONS:
    -q, --quiet           Suppress progress output (errors still shown)
    -v, --verbose         Show debug info
    -V, --version         Show version
```

**Exit codes:**

```rust
pub enum ExitCode {
    Success = 0,
    GeneralError = 1,
    NoResults = 2,       // Query returned nothing
    IndexMissing = 3,    // .cq/index.db not found
    ModelMissing = 4,    // Model not downloaded
    Interrupted = 130,   // Ctrl+C (128 + SIGINT)
}
```

**SIGINT handling (graceful shutdown):**

```rust
use std::sync::atomic::{AtomicBool, Ordering};

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

fn setup_signal_handler() {
    ctrlc::set_handler(|| {
        if INTERRUPTED.swap(true, Ordering::SeqCst) {
            // Second Ctrl+C: force exit
            std::process::exit(130);
        }
        // Note: eprintln! is not async-signal-safe, but ctrlc runs handler
        // in a separate thread (not signal context), so this is safe.
        eprintln!("\nInterrupted. Finishing current batch...");
    }).expect("Failed to set Ctrl+C handler");
}

fn check_interrupted() -> bool {
    INTERRUPTED.load(Ordering::SeqCst)
}

// In indexing loop:
for batch in chunks.chunks(BATCH_SIZE) {
    if check_interrupted() {
        eprintln!("Committing partial index...");
        break;
    }
    // Process batch...
}
```

**Parallel file parsing:**

```rust
use rayon::prelude::*;

// Note: tree_sitter::Parser::parse() takes &mut self, so each thread needs its own Parser.
// We create a fresh Parser per file. Parser::new() is cheap (~microseconds).
fn parse_files(files: &[PathBuf]) -> Vec<Chunk> {
    files
        .par_iter()  // Parallel iteration
        .flat_map(|path| {
            // Create parser per thread (Parser::parse needs &mut self)
            let parser = match Parser::new() {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!("Failed to create parser: {}", e);
                    return vec![];
                }
            };
            match parser.parse_file(path) {
                Ok(chunks) => chunks,
                Err(e) => {
                    tracing::warn!("Failed to parse {}: {}", path.display(), e);
                    vec![]
                }
            }
        })
        .collect()
}
```

**JSON output schema:**

```json
{
  "results": [
    {
      "file": "src/utils/retry.rs",
      "line_start": 24,
      "line_end": 45,
      "name": "retry_with_backoff",
      "signature": "pub async fn retry_with_backoff<T, E, F, Fut>(...)",
      "language": "rust",
      "chunk_type": "function",
      "score": 0.89,
      "content": "/// Retries an async operation..."
    }
  ],
  "query": "retry with backoff",
  "total": 2,
  "time_ms": 23
}
```

**cq init**

```
$ cq init

Initializing cq...
Downloading model (~547MB)... ████████████████████ done
Downloading tokenizer... done
Detecting hardware... NVIDIA RTX A6000 (CUDA)
Created .cq/

Run 'cq index' to index your codebase.
```

**cq doctor**

```
$ cq doctor

Runtime:
  [✓] Model: nomic-embed-text-v1.5
  [✓] Tokenizer: loaded
  [✓] Execution: CUDA (NVIDIA RTX A6000, 48GB)
  [✓] Test embedding: 12ms

Parser:
  [✓] tree-sitter: loaded
  [✓] Languages: rust, python, typescript, javascript, go

Index:
  [✓] Location: .cq/index.db
  [✓] Schema version: 1
  [✓] 847 chunks indexed (412 rust, 290 python, 145 typescript)
  [✓] Last updated: 2 hours ago

All checks passed.
```

**cq index**

```
$ cq index

Scanning files...
Found 156 files (89 .rs, 42 .py, 25 .ts)

Parsing...
├─ src/     [████████████████████] 89/89
├─ scripts/ [████████████████████] 42/42
└─ web/     [████████████████████] 25/25

Embedding (CUDA)...
[████████████████████] 847/847 chunks

Index complete:
  Functions: 623
  Methods: 224
  Skipped (>100 lines): 3

Time: 12.4s (68 chunks/sec)
```

**cq <query>**

```
$ cq retry with backoff

src/utils/retry.rs:24 (fn retry_with_backoff) [rust] [0.89]
─────────────────────────────────────────────────
/// Retries an async operation with exponential backoff.
pub async fn retry_with_backoff<T, E, F, Fut>(
    operation: F,
    max_attempts: u32,
    base_delay: Duration,
) -> Result<T, E>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<T, E>>,
{
    ...
}

scripts/api_client.py:156 (def fetch_with_retry) [python] [0.81]
─────────────────────────────────────────────────
def fetch_with_retry(self, url: str, max_retries: int = 3) -> Response:
    """Fetches URL with exponential backoff on failure."""
    ...

2 results (23ms)
```

**cq --lang rust <query>**

```
$ cq --lang rust database connection pool

src/db/pool.rs:12 (fn create_pool) [rust] [0.87]
─────────────────────────────────────────────────
/// Creates a connection pool with the given configuration.
pub fn create_pool(config: &DbConfig) -> Result<Pool<Postgres>> {
    ...
}

1 result (18ms)
```

---

## Configuration

```
~/.config/cq/config.toml     # global defaults
<project>/.cq/config.toml    # project overrides
```

```toml
# .cq/config.toml

[index]
# Patterns are per-language globs
include = [
    "src/**/*.rs",
    "lib/**/*.py",
    "src/**/*.ts",
]
exclude = [
    "target/**",
    "**/generated/**",
    "**/*_test.rs",
    "**/__pycache__/**",
    "**/node_modules/**",
]
max_chunk_lines = 100

[embedding]
# Auto-detected, but can force:
# provider = "cpu"  # or "cuda"
batch_size_gpu = 16
batch_size_cpu = 4

[query]
default_limit = 5
default_threshold = 0.3

[output]
color = true
```

---

## Directory Structure

**Global cache:**
```
~/.cache/cq/
├── models/
│   └── nomic-embed-text-v1.5/
│       ├── model.onnx
│       ├── tokenizer.json
│       └── manifest.json
```

**Per-project:**
```
.cq/
├── config.toml    # optional project config
├── index.db       # sqlite database
├── index.lock     # prevents concurrent indexing
└── .gitignore     # see below
```

**.cq/.gitignore contents:**
```
index.db
index.db-wal
index.db-shm
index.lock
```

---

## Dependencies

```toml
[package]
name = "cq"
version = "0.1.0"
edition = "2021"

[dependencies]
# CLI
clap = { version = "4", features = ["derive"] }

# Parsing (tree-sitter)
tree-sitter = "0.26"
tree-sitter-rust = "0.23"
tree-sitter-python = "0.23"
tree-sitter-typescript = "0.23"
tree-sitter-javascript = "0.25"
tree-sitter-go = "0.23"

# ML
# Note: ort 2.0 stable not released yet, use exact RC version
ort = { version = "2.0.0-rc.11", features = ["cuda", "tensorrt"] }
tokenizers = { version = "0.22", features = ["http"] }
hf-hub = "0.4"
ndarray = "0.16"

# Async
tokio = { version = "1", features = ["rt-multi-thread", "fs"] }

# Storage
rusqlite = { version = "0.31", features = ["bundled"] }

# Serialization
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"

# Utilities
blake3 = "1"
walkdir = "2"              # File enumeration (no symlink follow)
fs2 = "0.4"                # File locking
ctrlc = "3"                # SIGINT handling
rayon = "1"                # Parallel parsing
chrono = "0.4"             # Timestamps
glob = "0.3"
colored = "2"
indicatif = "0.17"
anyhow = "1"
thiserror = "2"
dirs = "5"
tracing = "0.1"
tracing-subscriber = "0.3"

# MCP server (Phase 3)
# async-trait = "0.1"     # Async trait methods
# futures = "0.3"         # Stream utilities

[dev-dependencies]
tempfile = "3"
insta = "1"

# Note: cc is a transitive build dependency of tree-sitter grammar crates.
# No need to declare it directly - cargo handles this automatically.
```

**Build notes:**
- tree-sitter grammars compile C code via `cc` crate (transitive dependency)
- Adds ~500KB base + ~2-3MB per grammar to binary
- Cross-compilation needs C toolchain for target
- Windows/macOS/Linux all supported

---

## Implementation Phases

### Phase 1: MVP

1. **Parser** - tree-sitter, Rust + Python + TypeScript + Go
2. **Embedder** - ort + tokenizers, CUDA/CPU detection, model download
3. **Store** - sqlite, BLOB, brute-force search
4. **CLI** - init, doctor, index, query, stats, --lang filter
5. **Eval** - 10 queries per language, measure recall

**Exit criteria:**
- `cargo install cq` works
- GPU used when available, CPU fallback works
- 8/10 test queries return relevant results per language
- Index survives Ctrl+C during indexing

### Phase 2: Polish

1. More chunk types (classes, structs, interfaces)
2. More languages (C, C++, Java, Ruby)
3. Path filtering (`cq "error" --path "src/api/**"`)
4. Hybrid search (embedding + name text match)
5. Watch mode (`cq watch`)
6. Stale file detection in doctor

### Phase 3: Integration

1. **MCP server** - `cq serve` for Claude Code (see MCP Integration section)
   - 3a: Core (`cq_search`, `cq_stats`, stdio transport)
   - 3b: Polish (`cq_similar`, `cq_index`, progress, SSE)
   - 3c: Production (pooling, health, metrics)
2. `--context N` to show surrounding code
3. VS Code extension (use MCP or direct integration)
4. Language server hints

### Phase 4: Scale

1. HNSW index for >50k chunks
2. Incremental embedding updates
3. Index sharing (team sync)

---

## Known Limitations

1. **Macros** - Sees invocation, not expanded code
2. **Large functions** - >100 lines skipped
3. **Negation** - "without X" not supported
4. **Multi-hop** - Single-chunk retrieval only
5. **Offline** - First run requires network for model download
6. **Error recovery** - tree-sitter tolerates broken code, but extracted chunks may be incomplete

---

## Security

1. **No telemetry** - Nothing leaves your machine except model download
2. **Local storage** - Embeddings stored locally only
3. **Embeddings are lossy** - Can't reconstruct code from embeddings, but can hint at content
4. **Model verification** - SHA256 checksum on download
5. **C FFI** - tree-sitter is widely audited (GitHub, editors)
6. **Path validation** - Files validated to stay within project root
7. **No symlink follow** - Prevents path traversal attacks
8. **File size limits** - Prevents memory exhaustion from large files
9. **Concurrent access** - File lock prevents index corruption

---

## MCP Integration

Model Context Protocol integration for Claude Code and other MCP-compatible clients.

### Overview

cq exposes semantic code search as an MCP server. This allows AI assistants to query the codebase semantically without multiple grep/glob round-trips.

**Why MCP?**
- Single tool call vs. multiple search iterations
- Structured results with scores and metadata
- Progress notifications for long operations
- Automatic discovery via config

### Server Mode

```
cq serve [OPTIONS]

OPTIONS:
    --transport <TYPE>    Transport type: stdio (default), sse
    --port <PORT>         Port for SSE transport (default: 3000)
    --project <PATH>      Project root (default: current directory)
```

**Startup behavior:**
1. Find project root (walk up looking for .cq/)
2. Open index (or return error if missing)
3. Load model lazily (on first search)
4. Listen for MCP messages

### Transport

**stdio (default)** - For subprocess spawning (Claude Code):
```json
// Request (stdin)
{"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {...}}

// Response (stdout)
{"jsonrpc": "2.0", "id": 1, "result": {...}}
```

**SSE** - For HTTP clients:
```
POST /message
Content-Type: application/json

{"jsonrpc": "2.0", ...}
```

### Tools

#### cq_search

Semantic code search. The primary tool.

```json
{
  "name": "cq_search",
  "description": "Search code semantically. Find functions/methods by concept, not just name. Example: 'retry with exponential backoff' finds retry logic regardless of naming.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "query": {
        "type": "string",
        "description": "Natural language description of what you're looking for"
      },
      "limit": {
        "type": "integer",
        "description": "Maximum results (default: 5, max: 20)",
        "default": 5
      },
      "threshold": {
        "type": "number",
        "description": "Minimum similarity score 0.0-1.0 (default: 0.3)",
        "default": 0.3
      },
      "language": {
        "type": "string",
        "enum": ["rust", "python", "typescript", "javascript", "go"],
        "description": "Filter by language (optional)"
      },
      "path_pattern": {
        "type": "string",
        "description": "Glob pattern to filter paths (e.g., 'src/api/**')"
      }
    },
    "required": ["query"]
  }
}
```

**Response:**
```json
{
  "results": [
    {
      "file": "src/utils/retry.rs",
      "line_start": 24,
      "line_end": 45,
      "name": "retry_with_backoff",
      "signature": "pub async fn retry_with_backoff<T, E, F, Fut>(...)",
      "language": "rust",
      "chunk_type": "function",
      "score": 0.89,
      "content": "/// Retries an async operation with exponential backoff.\npub async fn retry_with_backoff<T, E, F, Fut>(\n    operation: F,\n    max_attempts: u32,\n) -> Result<T, E>\nwhere\n    F: Fn() -> Fut,\n    Fut: Future<Output = Result<T, E>>,\n{\n    // ...\n}"
    }
  ],
  "query": "retry with exponential backoff",
  "total": 1,
  "time_ms": 23
}
```

#### cq_similar

Find code similar to a given chunk. Useful for finding related implementations.

```json
{
  "name": "cq_similar",
  "description": "Find code similar to a specific function/method. Pass file path and line number of existing code.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "file": {
        "type": "string",
        "description": "Path to file containing the reference code"
      },
      "line": {
        "type": "integer",
        "description": "Line number within the chunk"
      },
      "limit": {
        "type": "integer",
        "default": 5
      }
    },
    "required": ["file", "line"]
  }
}
```

**Implementation:** Look up chunk by file:line, use its embedding to search.

#### cq_stats

Index health and statistics.

```json
{
  "name": "cq_stats",
  "description": "Get index statistics: chunk counts, languages, last update time.",
  "inputSchema": {
    "type": "object",
    "properties": {}
  }
}
```

**Response:**
```json
{
  "total_chunks": 847,
  "total_files": 156,
  "by_language": {
    "rust": 412,
    "python": 290,
    "typescript": 145
  },
  "by_type": {
    "function": 623,
    "method": 224
  },
  "index_path": "/home/user/project/.cq/index.db",
  "index_size_mb": 12.4,
  "model": "nomic-embed-text-v1.5",
  "last_indexed": "2026-01-31T12:34:56Z",
  "schema_version": 1
}
```

#### cq_index

Trigger reindexing. Use sparingly - can be slow.

```json
{
  "name": "cq_index",
  "description": "Reindex the codebase. Only use when index is stale or missing files. Returns progress updates.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "force": {
        "type": "boolean",
        "description": "Force full reindex, ignore mtime cache",
        "default": false
      }
    }
  }
}
```

**Response (with progress):**
```json
{
  "status": "completed",
  "files_scanned": 156,
  "chunks_indexed": 847,
  "chunks_unchanged": 412,
  "chunks_updated": 35,
  "time_seconds": 12.4
}
```

**Progress notifications** (sent during indexing):
```json
{
  "jsonrpc": "2.0",
  "method": "notifications/progress",
  "params": {
    "progressToken": "index-1234",
    "value": {
      "kind": "report",
      "message": "Embedding chunks...",
      "percentage": 45
    }
  }
}
```

### Resources (Optional)

Expose indexed chunks as MCP resources for direct access:

```json
{
  "uri": "cq://chunks/src/utils/retry.rs:24",
  "name": "retry_with_backoff",
  "mimeType": "text/x-rust",
  "description": "pub async fn retry_with_backoff<T, E, F, Fut>(...)"
}
```

**List resources:** Returns top-level files with chunk counts.
**Read resource:** Returns full chunk content and metadata.

Deferred to Phase 3b - tools are higher priority.

### Prompts (Optional)

Pre-built prompts for common workflows:

```json
{
  "name": "find-similar",
  "description": "Find code similar to what's in your clipboard or selection",
  "arguments": [
    {
      "name": "code",
      "description": "The code to find similar implementations of",
      "required": true
    }
  ]
}
```

Deferred to Phase 3b.

### Configuration

**Claude Code setup** (`~/.claude/claude_code_config.json`):
```json
{
  "mcpServers": {
    "cq": {
      "command": "cq",
      "args": ["serve"],
      "env": {}
    }
  }
}
```

**Per-project override** (`.claude/settings.json` in project):
```json
{
  "mcpServers": {
    "cq": {
      "command": "cq",
      "args": ["serve", "--project", "."]
    }
  }
}
```

**Multi-project setup** (workspaces):
```json
{
  "mcpServers": {
    "cq-frontend": {
      "command": "cq",
      "args": ["serve", "--project", "./frontend"]
    },
    "cq-backend": {
      "command": "cq",
      "args": ["serve", "--project", "./backend"]
    }
  }
}
```

### Error Handling

**Index missing:**
```json
{
  "error": {
    "code": -32000,
    "message": "Index not found. Run 'cq init && cq index' first.",
    "data": {
      "type": "index_missing",
      "project": "/home/user/project"
    }
  }
}
```

**Index stale** (files changed since last index):
```json
{
  "result": {
    "results": [...],
    "_warning": "Index may be stale. 12 files modified since last index."
  }
}
```

**Model not downloaded:**
```json
{
  "error": {
    "code": -32001,
    "message": "Model not downloaded. Run 'cq init' first.",
    "data": {
      "type": "model_missing"
    }
  }
}
```

**Empty query:**
```json
{
  "error": {
    "code": -32602,
    "message": "Query cannot be empty",
    "data": {
      "type": "invalid_params"
    }
  }
}
```

### Implementation

**MCP protocol types:**
```rust
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Deserialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    id: Option<Value>,
    method: String,
    params: Option<Value>,
}

#[derive(Serialize)]
struct JsonRpcResponse {
    jsonrpc: String,
    id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Serialize)]
struct JsonRpcError {
    code: i32,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

// MCP-specific types
#[derive(Deserialize)]
struct InitializeParams {
    protocol_version: String,
    capabilities: Value,
    client_info: ClientInfo,
}

#[derive(Deserialize)]
struct ClientInfo {
    name: String,
    version: String,
}

#[derive(Serialize)]
struct InitializeResult {
    protocol_version: String,
    capabilities: ServerCapabilities,
    server_info: ServerInfo,
}

#[derive(Serialize)]
struct ServerCapabilities {
    tools: ToolsCapability,
}

#[derive(Serialize)]
struct ToolsCapability {
    list_changed: bool,
}

#[derive(Serialize)]
struct ServerInfo {
    name: String,
    version: String,
}

#[derive(Serialize)]
struct Tool {
    name: String,
    description: String,
    input_schema: Value,
}

// Tool argument types
#[derive(Deserialize)]
struct SearchArgs {
    query: String,
    limit: Option<usize>,
    threshold: Option<f32>,
    language: Option<String>,
    path_pattern: Option<String>,
}

#[derive(Deserialize)]
struct SimilarArgs {
    file: String,
    line: u32,
    limit: Option<usize>,
}

#[derive(Deserialize)]
struct IndexArgs {
    force: Option<bool>,
}
```

**Server struct:**
```rust
pub struct McpServer {
    store: Store,
    embedder: Option<Embedder>,  // Lazy loaded
    project_root: PathBuf,
}

impl McpServer {
    pub fn new(project_root: PathBuf) -> Result<Self>;

    // MCP protocol handlers
    pub async fn handle_initialize(&self, params: InitializeParams) -> InitializeResult;
    pub async fn handle_tools_list(&self) -> Vec<Tool>;
    pub async fn handle_tools_call(&mut self, name: &str, args: Value) -> Result<Value>;

    // Tool implementations
    fn search(&mut self, args: SearchArgs) -> Result<SearchResponse>;
    fn similar(&mut self, args: SimilarArgs) -> Result<SearchResponse>;
    fn stats(&self) -> Result<StatsResponse>;
    fn index(&mut self, args: IndexArgs) -> Result<IndexResponse>;

    // Ensure embedder is loaded (lazy init)
    fn ensure_embedder(&mut self) -> Result<&Embedder>;

    // Main request router
    async fn handle_request(&mut self, req: JsonRpcRequest) -> JsonRpcResponse {
        let result = match req.method.as_str() {
            "initialize" => self.handle_initialize(req.params).await,
            "tools/list" => Ok(serde_json::to_value(self.handle_tools_list().await)?),
            "tools/call" => self.handle_tools_call_dispatch(req.params).await,
            _ => Err(anyhow!("Unknown method: {}", req.method)),
        };

        match result {
            Ok(value) => JsonRpcResponse {
                jsonrpc: "2.0".into(),
                id: req.id,
                result: Some(value),
                error: None,
            },
            Err(e) => JsonRpcResponse {
                jsonrpc: "2.0".into(),
                id: req.id,
                result: None,
                error: Some(JsonRpcError {
                    code: -32000,
                    message: e.to_string(),
                    data: None,
                }),
            },
        }
    }
}
```

**Main loop (stdio):**
```rust
pub async fn serve_stdio(project_root: PathBuf) -> Result<()> {
    let mut server = McpServer::new(project_root)?;
    let stdin = BufReader::new(tokio::io::stdin());
    let mut stdout = tokio::io::stdout();

    let mut lines = stdin.lines();
    while let Some(line) = lines.next_line().await? {
        let request: JsonRpcRequest = serde_json::from_str(&line)?;
        let response = server.handle_request(request).await;
        let response_json = serde_json::to_string(&response)?;
        stdout.write_all(response_json.as_bytes()).await?;
        stdout.write_all(b"\n").await?;
        stdout.flush().await?;
    }
    Ok(())
}
```

### Dependencies (Additional)

```toml
# Add to Cargo.toml for MCP support
async-trait = "0.1"       # Async trait methods
futures = "0.3"           # Stream utilities
```

### Testing

**Manual testing:**
```bash
# Start server
echo '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}' | cq serve

# Search
echo '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"cq_search","arguments":{"query":"error handling"}}}' | cq serve
```

**Integration test:**
```rust
#[tokio::test]
async fn test_mcp_search() {
    let server = McpServer::new(test_project_path()).unwrap();
    let result = server.handle_tools_call("cq_search", json!({
        "query": "test query"
    })).await.unwrap();

    assert!(result["results"].is_array());
}
```

### Phase 3 Milestones

1. **3a: Core MCP** (MVP)
   - `cq serve` with stdio transport
   - `cq_search` tool
   - `cq_stats` tool
   - Basic error handling
   - Claude Code config docs

2. **3b: Polish**
   - `cq_similar` tool
   - `cq_index` tool with progress
   - SSE transport
   - Resources (optional)
   - Prompts (optional)

3. **3c: Production**
   - Connection pooling
   - Graceful shutdown
   - Health checks
   - Metrics/logging

---

## Changelog

- **0.10.0-draft (2026-01-31)**: Fixed remaining high-priority issues from v0.9 audit:
  - **Helpers**: Added `embedding_to_bytes()` and `bytes_to_embedding()` for BLOB serialization
  - **Traits**: Added `Display` and `FromStr` impls for `Language` and `ChunkType`
  - **Store**: Added struct definition with `conn: Connection` field
  - **CLI**: Added `serve` command to help text
  - **MCP types**: Added all protocol types (`JsonRpcRequest`, `JsonRpcResponse`, `Tool`, `SearchArgs`, etc.)
  - **MCP routing**: Added `handle_request()` method for dispatching JSON-RPC methods
- **0.9.0-draft (2026-01-31)**: Added comprehensive MCP Integration section:
  - **Server mode**: `cq serve` with stdio/SSE transports
  - **Tools**: `cq_search`, `cq_similar`, `cq_stats`, `cq_index` with full JSON schemas
  - **Configuration**: Claude Code setup, per-project overrides, multi-project workspaces
  - **Error handling**: Structured errors for missing index, stale data, invalid params
  - **Implementation**: Server struct, async handlers, lazy model loading
  - **Milestones**: Phase 3a/3b/3c breakdown
- **0.8.0-draft (2026-01-31)**: Fixed compilation errors and missing types from re-audit:
  - **Critical fixes**: `upsert_chunks_batch` now takes `&mut self`, fixed `needs_reindex` (removed invalid `.flatten()`), fixed `check_schema_version` type parsing (TEXT→i32)
  - **Schema**: Added `file_mtime INTEGER` column to chunks table
  - **Types**: Added missing struct definitions: `ChunkSummary`, `ChunkRow`, `ModelInfo`, `IndexStats`
  - **Thread safety**: Fixed parallel parsing - create Parser per thread (tree-sitter needs `&mut self`)
  - **Validation**: Added empty query validation in `embed_query()`
  - **Security**: Added model checksum verification skeleton (SHA256 with blake3)
  - **Docs**: Clarified ctrlc handler runs in thread (not signal context, so eprintln is safe)
- **0.7.0-draft (2026-01-31)**: Security and robustness overhaul from deep audit:
  - **Security**: Path validation (canonicalize + prefix check), symlinks skipped, file size limit (1MB)
  - **Concurrency**: File lock prevents concurrent `cq index`, schema/model version checks on open
  - **Performance**: Batch SQLite inserts (10x faster), parallel file parsing (rayon), mtime caching
  - **Robustness**: UTF-8 error handling (skip, don't abort), SIGINT graceful shutdown
  - **CLI**: Added `--force`, `--dry-run` flags, defined exit codes, JSON output schema
  - **Dependencies**: Added walkdir, fs2, ctrlc, rayon, chrono
  - **Docs**: Updated .gitignore to include WAL files and lock file
- **0.6.2-draft (2026-01-31)**: Post-audit fixes: Fixed ort execution provider imports (`ep::CUDA` not `CUDAExecutionProvider`), corrected model size (547MB), added SQLite pragma setup code, fixed search function closure issue, added batch_size to Embedder struct, fixed chunk ID format to include hash prefix, removed duplicate arrow function query, clarified token_type_ids not needed, pinned ort to exact RC version.
- **0.6.1-draft (2026-01-31)**: Audit fixes: Updated ort 2.x API (`try_extract_array`, `axis_iter`), improved query capture finding by index, enhanced TypeScript arrow function queries, fixed Go method detection, improved Python docstring extraction with `named_child`, clarified cc transitive dependency.
- **0.6.0-draft (2026-01-31)**: Replaced syn with tree-sitter for multi-language support. Added Python, TypeScript, JavaScript, Go. Updated dependencies. Simplified ExecutionProvider enum (removed CoreML/DirectML from MVP).
- **0.5.1-draft (2026-01-30)**: Verified TBDs: sentence_embedding is pre-pooled, use hf-hub crate for downloads.
- **0.5.0-draft (2026-01-30)**: Corrected ONNX tensor names. Fixed input dtype to INT32.
- **0.4.0-draft**: Use ort + tokenizers directly for GPU support.
- **0.3.0-draft**: Attempted fastembed-rs, discovered GPU limitation.
- **0.2.0-draft**: Doctor, prune, deduplication, versioning.
- **0.1.0-draft**: Initial design with Ollama.
