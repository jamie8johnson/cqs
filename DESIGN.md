# cq - Code Query

**Version:** 0.7.0-draft
**Updated:** 2026-01-31T03:00:00Z

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

fn ensure_model() -> Result<(PathBuf, PathBuf)> {
    // hf-hub handles caching automatically (~/.cache/huggingface/hub/)
    let api = Api::new()?;
    let repo = api.model(MODEL_REPO.to_string());
    
    let model_path = repo.get(MODEL_FILE)?;
    let tokenizer_path = repo.get(TOKENIZER_FILE)?;
    
    Ok((model_path, tokenizer_path))
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
    pub fn embed_query(&self, text: &str) -> Result<Embedding> {
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
        let version: i32 = self.conn
            .query_row("SELECT value FROM metadata WHERE key = 'schema_version'", [], |r| r.get(0))
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
```

**Embedding storage:**
- Store as BLOB (3072 bytes for 768 f32s)
- Little-endian byte order
- Compress in Phase 2 if needed

**Batch inserts (10x faster):**

```rust
fn upsert_chunks_batch(&self, chunks: &[(Chunk, Embedding)]) -> Result<usize> {
    let tx = self.conn.transaction()?;

    {
        let mut stmt = tx.prepare_cached(
            "INSERT OR REPLACE INTO chunks (id, file, language, chunk_type, name, signature, content, content_hash, doc, line_start, line_end, embedding, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)"
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
// Add to chunks table
// file_mtime INTEGER  -- Unix timestamp of file when indexed

fn needs_reindex(&self, path: &Path) -> Result<bool> {
    let current_mtime = path.metadata()?.modified()?
        .duration_since(std::time::UNIX_EPOCH)?.as_secs() as i64;

    let stored_mtime: Option<i64> = self.conn
        .query_row(
            "SELECT MAX(file_mtime) FROM chunks WHERE file = ?1",
            [path.to_string_lossy()],
            |r| r.get(0)
        ).ok().flatten();

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

fn parse_files(parser: &Parser, files: &[PathBuf]) -> Vec<Chunk> {
    files
        .par_iter()  // Parallel iteration
        .flat_map(|path| {
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

1. MCP tool for Claude Code
2. `--context N` to show surrounding code
3. VS Code extension
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

## Changelog

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
