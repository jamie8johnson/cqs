## Test Coverage (adversarial)

#### TC-ADV-1.29-1: `normalize_l2` silently returns NaN/Inf for non-finite input — no test
- **Difficulty:** easy
- **Location:** `src/embedder/mod.rs:1023-1030` (definition). Tests live at `src/embedder/mod.rs:1200-1233`. Suggested new tests in the same module.
- **Description:** `normalize_l2` is called on every raw ORT output (`src/embedder/mod.rs:911`) and receives whatever the model produces. If any element is NaN, `norm_sq = v.iter().fold(0.0, |acc, &x| acc + x * x)` becomes NaN, the `norm_sq > 0.0` check is false, and the original NaN values are returned unchanged. If any element is +Inf, `norm_sq = Inf`, `inv_norm = 1.0/Inf = 0.0`, and the returned vector is all zeros — silently blanked. Neither case is tested today (existing tests cover unit-vector, 3-4-5, zero, empty). The HNSW search path has a NaN guard (`src/hnsw/search.rs:82`) but the `search_filtered` brute-force path, reranker path, and neighbors path all consume these embeddings without a guard — a degenerate ONNX output silently corrupts search results.
- **Suggested fix:** Add three tests in the existing `tests` module:
  - `test_normalize_l2_nan_propagates` — `normalize_l2(vec![1.0, f32::NAN, 0.0])` — pin current behavior (NaN out) OR change the contract to fail. Either way, don't leave it untested.
  - `test_normalize_l2_inf_collapses_to_zero` — `normalize_l2(vec![1.0, f32::INFINITY, 2.0])` — pin the current Inf → 0-vec collapse.
  - `test_normalize_l2_neg_inf` — same for `f32::NEG_INFINITY`.

#### TC-ADV-1.29-2: `embed_batch` does not validate ORT-returned tensor for NaN/Inf before returning `Embedding`
- **Difficulty:** medium
- **Location:** `src/embedder/mod.rs:903-914` (the pooled→normalized→Embedding::new path). No existing test covers the "model emits NaN" case.
- **Description:** `Embedding::try_new` rejects non-finite values, but `embed_batch` uses `Embedding::new(normalize_l2(v))` (line 911), which is the unchecked constructor. Combined with the `normalize_l2` NaN/Inf passthrough above, a broken ONNX model (observed in: quantization bugs, model-weight bit rot, corrupt model download with matching checksum, FP16 overflow on long inputs) produces `Embedding` values that flow through `embed_query` → query cache → HNSW search → brute-force scoring with no canary. `is_finite` is checked at HNSW search (line 82) — but the disk cache `QueryCache::put` (`src/cache.rs:1227`) stores the NaN embedding to disk, poisoning future queries across processes.
- **Suggested fix:** Add a test that monkey-patches a pooling result with a NaN element, then asserts `embed_batch` either (a) errors, or (b) produces a finite embedding — whichever is the chosen contract. Specifically:
  - `test_embed_batch_rejects_nan_pool_output` via a trait/mock or by poking a `normalize_l2` call path; assert `Result::Err(EmbedderError::InferenceFailed(_))` or similar.
  - In parallel, add `test_query_cache_put_rejects_non_finite_embedding` in `src/cache.rs` so even if embedder is misbehaving, the cache layer is a backstop.

#### TC-ADV-1.29-3: Daemon socket handler — zero adversarial tests for request shapes
- **Difficulty:** medium
- **Location:** `src/cli/watch.rs:160-406` (`handle_socket_client`). Tests start at line 2637, but none of them exercise `handle_socket_client`. Integration tests in `tests/daemon_forward_test.rs` only cover the CLI→daemon happy path (notes list, ping).
- **Description:** The daemon socket is the hot path for every agent query and has rich adversarial surface. None of the following are tested:
  - **1 MiB boundary**: request exactly 1,048,577 bytes should produce `"request too large"` error (logic at `src/cli/watch.rs:198-207`).
  - **Malformed JSON**: trailing garbage after a valid object, UTF-16 BOM prefix, JSON with NaN literals (`{"command":"ping","args":[]}NaN`), empty line, whitespace only.
  - **Missing `command` field**: `{"args":[]}` should produce `"missing 'command' field"` (line 304-312).
  - **Non-string args**: `{"command":"notes","args":[{}, null, 42]}` — rejects with `"args contains non-string elements"` (line 248-263). Today this is only covered structurally by code review.
  - **Oversized single arg**: `{"command":"search","args":["<500KB of base64>"]}` — within 1 MiB line but exhausts memory downstream.
  - **NUL byte in args**: the batch path validates NUL bytes (`src/cli/batch/mod.rs:579`) — but `handle_socket_client` relies on `dispatch_line` downstream to catch this; no integration test pins that boundary.
  - **Notes secret-redaction**: `{"command":"notes","args":["add","secret text"]}` — the log line should show `notes/add` (line 279-285), not the full arg. Regression risk that isn't test-pinned.
- **Suggested fix:** Create `tests/daemon_adversarial_test.rs`. Wire up a fixture that constructs `BatchContext` + runs `handle_socket_client` against a `UnixStream` pair (the existing `MockDaemon` in `tests/daemon_forward_test.rs` is the wrong shape — that's a mock daemon for the CLI; we need the reverse). Add one test per case above; assert response envelope matches expected error code or payload. A NUL-byte-in-args case should verify the client receives an invalid-input error rather than the command being executed with a mangled string.

#### TC-ADV-1.29-4: `parse_unified_diff` has no test for empty-file / whitespace-only hunk headers / duplicate `+++` lines
- **Difficulty:** easy
- **Location:** `src/diff_parse.rs:33-108` (definition), tests at `tests/diff_parse_test.rs` + `src/diff_parse.rs:110-237`.
- **Description:** Existing tests cover basic, new-file, deleted, binary, multiple hunks, count-omitted, empty, rename, u32-overflow, no-b-prefix. Missing:
  - **Two `+++` lines in a row without any hunk headers between them** — current code just overwrites `current_file` and does not warn; a diff like `+++ b/a.rs\n+++ b/b.rs\n@@ -1 +1 @@\n+x` will attribute the hunk to `b/b.rs` silently. Pin this behavior or reject.
  - **`@@` hunk header before any `+++` line** — dropped on the floor because `current_file = None`. No test.
  - **Only-whitespace diff input** (`"   \n\n\n"`) — currently returns empty Vec (via `lines()`) but not pinned.
  - **Hunk header with extra spaces inside** (`@@  -10,3  +10,5  @@`) — regex `\+(\d+)` requires exactly one space before `+`; the parser will silently drop the hunk. Not tested.
  - **CRLF-only line endings in middle of diff** (mixed with LF) — the `contains('\r')` check normalizes all `\r` to `\n`, but mid-hunk `\r` in the `+ line content` would double-normalize and change byte positions. Not tested.
- **Suggested fix:** Add to `tests/diff_parse_test.rs`:
  - `test_parse_unified_diff_double_plus_plus_line_uses_last` — pin last-wins behavior.
  - `test_parse_unified_diff_orphan_hunk_header_dropped` — hunk without preceding file is dropped.
  - `test_parse_unified_diff_hunk_header_extra_spaces` — pin current drop-on-floor behavior.
  - `test_parse_unified_diff_whitespace_only_input` — returns empty.

#### TC-ADV-1.29-5: `parse_notes_str` — no test for malformed TOML escapes, non-ASCII text, or oversized mentions array
- **Difficulty:** easy
- **Location:** `src/note.rs:325-348`. Existing tests cover happy path, clamping (not NaN — TC-ADV-6 in triage), empty file, stable IDs, MAX_NOTES truncation, proptest no-panic on 500-byte random input.
- **Description:** `parse_notes_str` accepts user-authored TOML. The proptest fuzz (`\\PC{0,500}`) won't hit these:
  - **A `[[note]]` with `mentions = [...]` containing 10,000+ strings**: each stored unchanged on `Note`. No per-note cap, no per-mentions cap. A malicious/mis-generated notes.toml can produce a Note with millions of mentions; `path_matches_mention` runs O(n_mentions × n_candidates) per query, DoS.
  - **`text = ""`** (empty string) — not rejected. The trimmed text is empty; hash of empty bytes is deterministic but conflicts with all other empty notes.
  - **`text = "\0\0\0"` (embedded NUL)** — accepted; when later written to a log line or daemon response, NUL may truncate downstream.
  - **`sentiment = "0.5"` (string instead of float)** — returns `NoteError::Toml` with a parse error message that leaks the raw TOML in the daemon error envelope. Not tested for redaction.
- **Suggested fix:** Add tests in `src/note.rs::tests`:
  - `test_parse_notes_str_huge_mentions_array` — 100k mentions on one note; assert it is parsed OR pin a cap. Recommend: cap mentions at e.g. 100 per note with warn.
  - `test_parse_notes_str_empty_text_rejected_or_kept` — pin behavior.
  - `test_parse_notes_str_nul_in_text` — assert NUL passes through verbatim; log/emit contract separately.

#### TC-ADV-1.29-6: HNSW `load_with_dim` — no test for id_map containing non-string JSON values
- **Difficulty:** easy
- **Location:** `src/hnsw/persist.rs:619-629` (id_map load). Existing tests at line 897+ cover oversized graph/data/id_map, missing checksum, dim mismatch, rebuild path.
- **Description:** The id_map is deserialized as `Vec<String>` via `serde_json::from_reader`. Several corrupt-but-parseable shapes are untested:
  - **id_map containing 10M zero-length strings** (each is `""` — 2 bytes in JSON). The file stays well under `MAX_ID_MAP_ENTRIES` (10M) cap but at 10M entries × avg 64 bytes overhead is 640 MB RAM. What happens when `id_map[5_000_000].clone()` hits a zero-string? That chunk never matches any chunk in SQLite — silent zero-result search, no warning.
  - **id_map with duplicate strings**: two entries `["chunk1", "chunk1"]` pointing to the same chunk id. The HNSW graph has two distinct nodes at position 0 and 1. Search returns `chunk1` twice with potentially different scores — duplicate result, breaks RRF downstream.
  - **id_map with strings containing embedded `\n` or `\0`**: survives deserialization, passes the `chunks_fts MATCH ?1` filter, but the `.hnsw.ids` JSON file round-trips — and the chunk_id becomes a lookup key in SQL. Injection surface if the id is later interpolated anywhere.
- **Suggested fix:** Add to `src/hnsw/persist.rs::tests`:
  - `test_load_rejects_duplicate_ids_in_id_map` — pin current behavior (duplicates accepted) or add a dedup check at load.
  - `test_load_rejects_empty_string_ids_in_id_map` — assert warn or error.
  - `test_load_rejects_nul_in_id_map_entry` — assert safety behavior.

#### TC-ADV-1.29-7: `embedding_slice` does not validate that decoded floats are finite — no test for NaN bytes in DB
- **Difficulty:** easy
- **Location:** `src/store/helpers/embeddings.rs:32-42`. Existing tests cover only size mismatch (3 cases).
- **Description:** `embedding_slice` validates byte length and casts to `&[f32]`. Any 4-byte sequence `0xFF 0xFF 0x7F 0x7F` is a valid NaN; `0x7F 0x80 0x00 0x00` is +Inf. If the SQLite embedding BLOB column is bit-rotted or written by a buggy embedder (see TC-ADV-1.29-2), `embedding_slice` silently returns NaN/Inf which flow directly into `score_candidate` (brute force path in `search_filtered`). `score_candidate` does have a NaN guard (test `score_candidate_nan_embedding_filtered` exists) — but the dot-product intermediates that feed it are computed every call. A whole-corpus NaN corruption from a single bad reindex produces zero results with no structured error.
- **Suggested fix:** Add to `src/store/helpers/embeddings.rs::tests`:
  - `test_embedding_slice_returns_nan_bytes_verbatim` — pin current passthrough behavior (the test author's choice: change contract or document it).
  - `test_bytes_to_embedding_nan_input` — same shape.
  - Recommend adding a `#[cfg(debug_assertions)]` sanity check that logs a warn-once if any decoded float is non-finite, since this is impossible under normal operation (the embedder always normalizes to unit length).

#### TC-ADV-1.29-8: `dispatch_line` shell_words tokenizer — no test for arg with ANSI escape sequences
- **Difficulty:** easy
- **Location:** `src/cli/batch/mod.rs:557-621`. Existing tests at line 2129+ cover NUL bytes in double-quoted args (P2 #51) and unbalanced quotes.
- **Description:** `dispatch_line` logs `args_len` (P3 #138 dropped the args_preview) but the full line passed downstream to `BatchInput::try_parse_from` is still parsed by clap with `args_preview`-style untrimmed tokens. If an agent sends `search "\x1b[2J\x1b[Hmalicious"` (ANSI clear screen + home cursor), the resulting error message flows to the client's terminal via tracing. Tested: NUL bytes (rejected). Untested: other C0 control chars (`\x07` BEL, `\x1b` ESC, `\x08` BS), `\r` as line separator within an arg, `\t` TAB inside a token.
- **Suggested fix:** Add to `src/cli/batch/mod.rs::tests`:
  - `test_dispatch_line_rejects_ansi_escape_in_arg` — pin either pass-through or rejection.
  - `test_dispatch_line_rejects_bel_in_arg` — same.
  - `test_dispatch_line_cr_in_arg_treated_as_arg_char_not_separator` — confirm single-line parsing still holds.

#### TC-ADV-1.29-9: `SpladeEncoder::encode` raw-logits path — no test for Inf-valued logits input
- **Difficulty:** easy
- **Location:** `src/splade/mod.rs:540-572` (encode, raw-logits branch). Tests at `src/splade/mod.rs:874+`.
- **Description:** Line 557: `pooled = logits.fold_axis(Axis(0), f32::NEG_INFINITY, |&a, &b| a.max(b))`. If `b` is +Inf, `a.max(+Inf) = +Inf`. Then line 564: `activated = (1.0 + val.max(0.0)).ln()` with `val = Inf` gives `activated = Inf`. Line 565: `Inf > self.threshold` → `true`, so the token is emitted with `Inf` weight. The resulting `SparseVector` then flows through `SpladeEncoder::search_with_filter`, which sums weighted dot products — Inf * anything = Inf, poisoning the entire score hash map. Silent corruption, no warning, no panic (the NaN branch actually filters via `> threshold == false`, but Inf passes the comparison).
- **Suggested fix:** Add to `src/splade/mod.rs::tests`:
  - `test_encode_rejects_inf_in_pooled_logits` (or sanitizes them) — pin whichever contract.
  - `test_encode_rejects_nan_in_pooled_logits` — the NaN path is "silently dropped" today; pin that or fail loudly.
  - Downstream: `test_splade_search_with_inf_weighted_sparse_vector` in `src/splade/index.rs`.

#### TC-ADV-1.29-10: `parse_unified_diff` called on 50 MB diff — no DoS test
- **Difficulty:** medium
- **Location:** `src/diff_parse.rs:33-108`, called from `src/cli/commands/graph/impact_diff.rs:39`, `src/review.rs:85`, `src/ci.rs:93`, `src/cli/batch/handlers/graph.rs:399`.
- **Description:** Upstream `MAX_DIFF_SIZE = 50MB` (`src/cli/commands/mod.rs:512`) caps stdin, but `parse_unified_diff` accepts any `&str` and builds a `Vec<DiffHunk>` with one allocation per hunk header matched. A 50 MB diff with a hunk header on every line (CRLF-normalized doubles memory briefly: `input.replace("\r\n", "\n").replace('\r', "\n")`) — hundreds of thousands of hunks, each allocating a `PathBuf` via `PathBuf::from(file.as_str())` (line 99). No cap on `hunks.len()`. `map_hunks_to_functions` has its own cap via `CQS_IMPACT_MAX_CHANGED_FUNCTIONS` (default 500), but that applies after `parse_unified_diff` has already built and returned the huge Vec. No test exercises the 50MB boundary or the "one hunk per line" worst case.
- **Suggested fix:** Add a test in `tests/diff_parse_test.rs`:
  - `test_parse_unified_diff_large_input_bounded` — construct 10 MB of `@@ +1,1 @@\n` lines with a leading `+++ b/foo.rs\n`, parse, assert Vec length matches and that memory usage stays bounded (e.g., under 100 MB wall-clock). Or, if a hard cap is desired, add `MAX_HUNKS` and pin it.
  - Currently `parse_unified_diff` also loses the performance feedback signal (no `tracing::info!` with hunk count after parse) so operators wouldn't see "10k hunks in one diff" in the journal.

## Summary

10 findings filed. Highest-impact gaps are (1) the `normalize_l2` NaN/Inf passthrough into disk cache and downstream scoring, (2) zero adversarial tests on the daemon socket handler (a production hot path handling untrusted JSON), and (3) the SPLADE raw-logits Inf propagation into sparse-vector score fusion. The diff parser has good coverage but misses some edge shapes that affect downstream review/impact commands.
