# `cqs train-data` Design Spec

## Purpose

Generate training data for LoRA fine-tuning E5-base-v2 on code search. Walks git history of multiple repos, pairs commit messages with changed functions, selects BM25 hard negatives. Output: JSONL triplets for `sentence-transformers` `MultipleNegativesRankingLoss`.

## Command

```bash
cqs train-data \
  --repos /path/to/cqs /path/to/tokio /path/to/serde \
  --output training_data.jsonl
```

### Flags

| Flag | Default | Description |
|------|---------|-------------|
| `--repos` | required | One or more repo paths |
| `--output` | required | Output JSONL file path |
| `--max-commits` | 0 (all) | Cap commits per repo |
| `--min-msg-len` | 15 | Skip short commit messages |
| `--max-files` | 20 | Skip commits touching more files (entire commit skipped) |
| `--dedup-cap` | 5 | Max triplets per unique function content hash |
| `--resume` | false | Continue from checkpoint |
| `--verbose` | false | Per-commit debug logging |

## Pipeline

### Per repo:

**Phase 1: Build BM25 index (one-time)**
1. Parse all source files at HEAD with tree-sitter → function name + content + BLAKE3 content hash
2. Build BM25 index over function content. Each function is a document, terms are whitespace-split lowercase tokens. IDF computed across the function corpus (downweights common terms like `fn`, `self`, `return`).
3. This index is used for hard negative selection

**Phase 2: Walk git history**
1. `git log --format="%H" --no-merges` → list of commit SHAs
2. For each commit:
   a. `git diff-tree --root --no-commit-id -r -p {commit}` → unified diff with hunks (`--root` handles the initial commit which has no parent)
   b. Parse diff output to extract: file paths, hunk line ranges (added/modified)
   c. For each changed file with a supported extension:
      - `git show {commit}:{path}` → file content at that commit (skip if > 50MB, matching `parse_file` MAX_FILE_SIZE)
      - Parse with tree-sitter via **new `Parser::parse_source()`** method (not `parse_file` — no disk read)
      - Extract function spans (name, start_line, end_line, content)
      - Intersect hunk line ranges with function spans → changed functions
   d. For each changed function:
      - Build query from commit message (see Query Normalization)
      - Select negatives via BM25 index (see Negative Selection)
      - Emit triplet to output stream

### Requires new Parser API

`Parser::parse_file()` reads from disk. This pipeline needs to parse in-memory content from `git show`. Add:

```rust
pub fn parse_source(
    &self,
    source: &str,
    language: Language,
    path: &Path,  // for chunk origin metadata (Chunk.file field)
) -> Result<Vec<Chunk>, ParserError>
```

`language` is passed explicitly (caller determines it from file extension before calling). `path` is metadata only — used to populate `Chunk.file`, not for language detection. Factor the file-read out of `parse_file` — it calls `parse_source` internally.

## Diff Hunk → Function Mapping

Parse unified diff output from `git diff-tree -p` to extract hunk headers (`@@ -a,b +c,d @@`). The `+c,d` range gives the added/modified line range in the new (commit-side) file.

For each function span from tree-sitter:
- If `hunk_start <= fn_end && hunk_end >= fn_start` → function was changed
- A single hunk spanning two functions → both are changed
- Changes outside any function (imports, module-level code) → skipped (no function to pair)
- Nested functions (closures inside fn): attribute to the outermost function

## Query Normalization

Strip commit message prefixes to approximate search intent.

**Conventional commit prefixes** (case-insensitive):
```
^(feat|fix|refactor|chore|docs|test|ci|style|perf|build|revert)(\(.*?\))?[!]?:\s*
```

**Leading action verbs** (after prefix stripping):
```
^(add|remove|delete|update|move|rename|extract|merge|split|rewrite|simplify|
  clean|cleanup|improve|replace|introduce|deprecate|drop|bump|implement|
  handle|support|allow|enable|disable|apply|use|wip)\s+
```

**Trailing noise:**
```
\s*\(#\d+\)\s*$   # PR references: (#456)
\s*#\d+\s*$       # Issue references: #123
```

Both `query` (normalized) and `raw_query` (original first line) are emitted.

## Negative Selection

**BM25 hard negatives from HEAD index:**
- For each normalized query, score all functions in the BM25 index (term frequency overlap between query tokens and function content tokens)
- Exclude the positive function (by BLAKE3 content hash — same hash algorithm used by the cqs Store)
- **Content hash guard:** Also exclude any candidate whose content hash matches the positive's hash. Catches renamed-but-identical functions that would be false negatives.
- Take top-3 surviving functions as negatives
- If fewer than 3 available, pad with random functions from the repo

**Known limitation:** BM25 index is built from HEAD, but positives come from historical commits. Functions deleted/renamed between commit and HEAD won't appear as candidates. The content hash guard prevents the worst failure mode (identical content as positive/negative pair).

## Output Format

One JSONL line per triplet, streamed (not buffered):

```jsonl
{
  "query": "config parser timeout",
  "raw_query": "fix(parser): config parser timeout (#234)",
  "positive": "fn parse_config(path: &Path) -> Result<Config> { ... }",
  "negatives": [
    "fn validate_schema(input: &str) -> bool { ... }",
    "fn load_defaults() -> Config { ... }",
    "fn merge_configs(base: &Config, override: &Config) -> Config { ... }"
  ],
  "repo": "cqs",
  "commit": "abc123def",
  "file": "src/config.rs",
  "function_name": "parse_config",
  "language": "rust",
  "files_changed": 3,
  "msg_len": 35,
  "diff_lines": 12,
  "function_size": 45,
  "commit_date": "2026-03-15"
}
```

## Deduplication

- Track functions by BLAKE3 content hash (same algorithm as cqs Store)
- Cap at `--dedup-cap` triplets per unique hash (default 5)
- Simple counter — first N occurrences emitted, rest skipped
- Different queries for same function content = useful signal, so the cap is per-hash not per-name

## Checkpointing

- Output file opened in append mode (crash-safe for completed lines)
- Single checkpoint file next to output (e.g., `training_data.jsonl.checkpoint`), one line per repo: `{repo_path}\t{last_completed_commit_sha}`
- Write checkpoint **after** each commit completes (standard at-least-once semantics)
- On crash: last commit may be partially written (incomplete JSONL line). On `--resume`: truncate any incomplete trailing line, skip commits up to and including the checkpoint SHA
- If a commit's triplets were partially written before crash, `--resume` re-processes that commit. Content hash dedup cap catches exact duplicates.

## Error Handling

| Condition | Action |
|-----------|--------|
| Shallow clone | Detect via `git rev-parse --is-shallow-repository`. Warn, process available history. |
| Binary file in diff | Skip (no supported extension) |
| Non-UTF-8 file content | `git show` returns raw bytes. Skip with `tracing::warn!` |
| `git show` fails (missing object) | Skip file, warn, continue |
| Tree-sitter parse failure | Use partial tree results, warn |
| Submodule pointer change | Detected by file mode 160000 in diff-tree output, or "Subproject commit" in patch content. Skip. |
| Root commit (no parent) | Handled by `--root` flag on `git diff-tree`. |
| File > 50MB from `git show` | Skip (matches `parse_file` MAX_FILE_SIZE guard). |
| Commit touches > `--max-files` files | Skip entire commit |
| Message length < `--min-msg-len` | Skip entire commit |
| Empty diff (metadata-only commit) | Skip |
| Repo path doesn't exist or isn't a git repo | Error, skip repo, continue to next |

All warnings use structured `tracing::warn!` with repo/commit/file fields.

## Tracing

```rust
let _span = tracing::info_span!("train_data", repos = repos.len()).entered();
// Per repo:
let _span = tracing::info_span!("train_data_repo", repo = %path.display(), commits).entered();
// Progress every 100 commits:
tracing::info!(processed = n, emitted = triplets, skipped, "Progress");
```

**Per-repo summary:**
```
cqs: 2847 commits, 1923 processed, 924 skipped (412 merge, 287 short msg, 225 bulk), 4521 triplets, 12 parse failures
```

**Final summary:**
```
Total: 14,832 triplets across 3 repos. Languages: rust 8421, python 3102, typescript 3309.
Top functions: parse_config (5 triplets, capped), handle_request (5), ...
```

## Tests

### Unit tests
- Diff hunk parsing: extract hunk ranges from unified diff output
- Hunk → function intersection: exact match, partial overlap, multi-function hunk, outside-function change, nested closures
- Query normalization: conventional commits, verb stripping, PR references, edge cases (empty after strip, all-verb message)
- BM25: positive excluded from negatives (by content hash), fallback to random when < 3 candidates
- Content hash dedup: cap enforcement, counter behavior
- JSONL serialization: round-trip, all fields present

### Integration tests
- Git fixture repo with 5-10 scripted commits:
  - Simple single-file change
  - Multi-file commit
  - Root commit (no parent)
  - File deleted in later commit but present in earlier
  - Function renamed between commits
- Verify exact triplet output for each case
- Verify checkpoint file written correctly
- Verify `--resume` skips completed commits

### End-to-end
- Run on cqs repo, verify valid JSONL
- Spot-check 10 random triplets: query makes sense, positive matches, negatives are keyword-similar but different

## Performance Estimate

Per repo (5000 commits, 10 files/commit average):
- `git log`: ~10ms (one call)
- `git diff-tree -p` per commit: ~5ms × 5000 = ~25s
- `git show` per file: ~2ms × 50k = ~100s
- Tree-sitter parse per file: ~1ms × 50k = ~50s
- BM25 per triplet: ~1ms × 15k = ~15s
- **Total: ~3-4 minutes per repo**

Memory: BM25 index (~10MB for 50k functions), streaming JSONL output (negligible).

## Not In Scope (V1)

- Per-commit BM25 index (too expensive)
- Synthetic query generation via LLM
- Docstring → function pairs (separate data source, different pipeline)
- Automatic train/val/test splitting (done at training time by repo field)
- Parallelism across repos (sequential is fine for 5-10 repos)
