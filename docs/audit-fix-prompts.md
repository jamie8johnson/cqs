# Audit Fix Prompts — P1 (post-v1.27.0)

Each section below is a self-contained fix prompt: file:line, current code, replacement code, why. Source: `docs/audit-triage.md`. Detail in `docs/audit-findings.md`.

These are the **P1** items only (26 fixes). P2/P3/P4 follow in subsequent passes.

---

## Group A — Parser / Algorithm (independent files)

### A.1 — Parser: tighten `line_looks_comment_like` so `#[derive]`, `#include`, `#define`, `*ptr` don't leak into doc (P1 #3)

**File:** `src/parser/chunk.rs:259-276` (`line_looks_comment_like`)

**Why:** The current predicate accepts any line whose `trim_start()` begins with `#`, `*`, etc. — so `#[derive(Debug)]` (Rust attribute), `#include`/`#define` (C preprocessor), `*ptr = 0` (C deref), `#region` (C# pragma) all qualify as "comment-like" and get pulled into the next short chunk's `doc` field by `extract_doc_fallback_for_short_chunk`. The fallback was added (PR #1040) for the truncated-gold lever — adding noise to short Rust struct chunks is an outright regression.

**Current code (verbatim, re-read first):**

```rust
fn line_looks_comment_like(line: &str) -> bool {
    let t = line.trim_start();
    t.is_empty()
        || t.starts_with("//")
        || t.starts_with("--")
        || t.starts_with('#')
        || t.starts_with("/*")
        || t.starts_with("*/")
        || t.starts_with('*')
        || t.starts_with("<!--")
        || t.starts_with("<%--")
        || t.starts_with("(*")
}
```

**Replacement code:**

Tighten the bare-`#` and bare-`*` arms so they require a separator (`#` followed by ` ` or end-of-line; `*` followed by ` `, `/`, or end-of-line). Other prefixes are unambiguous.

```rust
fn line_looks_comment_like(line: &str) -> bool {
    let t = line.trim_start();
    if t.is_empty() {
        return true;
    }
    // Multi-char comment introducers (unambiguous)
    if t.starts_with("//")
        || t.starts_with("--")
        || t.starts_with("/*")
        || t.starts_with("*/")
        || t.starts_with("<!--")
        || t.starts_with("<%--")
        || t.starts_with("(*")
    {
        return true;
    }
    // Single-char `#` is comment in sh/python/ruby ONLY when followed by whitespace
    // or alone. Reject `#[derive]`, `#include`, `#define`, `#pragma`, `#region`, `#!`.
    if let Some(rest) = t.strip_prefix('#') {
        return rest.is_empty() || rest.starts_with(' ') || rest.starts_with('\t');
    }
    // Single-char `*` is the inner-line marker of /** ... */ blocks ONLY when
    // followed by whitespace or alone. Reject `*ptr = 0`, `*x * y`, etc.
    if let Some(rest) = t.strip_prefix('*') {
        return rest.is_empty() || rest.starts_with(' ') || rest.starts_with('\t');
    }
    false
}
```

**Tests to add (in the existing `doc_fallback_tests` mod):**

```rust
#[test]
fn line_looks_comment_like_rejects_attribute_and_preprocessor_lines() {
    for line in [
        "#[derive(Debug, Clone)]",
        "#[serde(rename = \"foo\")]",
        "#[cfg(test)]",
        "#[allow(dead_code)]",
        "#include <stdio.h>",
        "#define MAX 100",
        "#pragma once",
        "#region Helpers",
        "#endregion",
        "#![feature(let_chains)]",
        "*ptr = 0;",
        "*x = y * z;",
    ] {
        assert!(
            !line_looks_comment_like(line),
            "expected NOT comment-like: {line:?}"
        );
    }
}

#[test]
fn line_looks_comment_like_still_accepts_real_comment_prefixes() {
    for line in [
        "# python comment",
        "# ",
        "#",
        "// rust",
        "/// outer doc",
        "/* block",
        " * inner of /** */ block",
        "* ",
        "*",
    ] {
        assert!(line_looks_comment_like(line), "expected comment-like: {line:?}");
    }
}

#[test]
fn rust_short_struct_after_derive_keeps_doc_none() {
    let content = "#[derive(Debug)]\nstruct Tiny { a: u32 }\n";
    let file = write_temp_file(content, "rs");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let chunk = chunks.iter().find(|c| c.name == "Tiny").expect("Tiny chunk");
    assert!(
        chunk.doc.is_none(),
        "fallback must not capture #[derive] line as doc, got: {:?}",
        chunk.doc
    );
}

#[test]
fn c_short_function_after_include_keeps_doc_none() {
    let content = "#include <stdio.h>\n#define X 1\nint main(void) { return 0; }\n";
    let file = write_temp_file(content, "c");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    if let Some(chunk) = chunks.iter().find(|c| c.name == "main") {
        assert!(
            chunk.doc.is_none(),
            "fallback must not capture #include/#define as doc, got: {:?}",
            chunk.doc
        );
    }
}
```

---

### A.2 — Parser: walk-back loop blank-line budget bug (P1 #4)

**File:** `src/parser/chunk.rs:311-327` (the walk-back + strip-leading-blanks loops in `extract_doc_fallback_for_short_chunk`)

**Why:** The walk-back counts blank lines toward `FALLBACK_DOC_MAX_LINES`. Then the strip-leading-blanks loop discards exactly those blank lines. Net effect: real comments past a blank gap are silently truncated. Documented failing case: SQL `CREATE TABLE` with leading `--` comments separated by 7+ blank lines from the table — only one comment line lands in `doc`, the earlier ones are dropped.

**Current code:**

```rust
    // Walk back from the line immediately preceding the chunk, gathering
    // contiguous comment-like lines. Stop at the first non-comment-like line.
    let mut start = lines.len();
    let mut taken = 0usize;
    while start > 0 && taken < FALLBACK_DOC_MAX_LINES {
        let candidate = lines[start - 1];
        if !line_looks_comment_like(candidate) {
            break;
        }
        start -= 1;
        taken += 1;
    }
    // Strip leading blank lines from the selection so we don't return an
    // empty/whitespace-only `doc`.
    while start < lines.len() && lines[start].trim().is_empty() {
        start += 1;
    }
```

**Replacement code:**

Track non-blank "comment lines" against the budget; allow runs of blank lines without consuming budget but cap them so a malformed file can't stall.

```rust
    // Walk back from the line immediately preceding the chunk. Spend the
    // FALLBACK_DOC_MAX_LINES budget only on non-blank comment-like lines so
    // a blank gap between the chunk and the leading comment block doesn't
    // silently truncate the captured doc. Cap the run of consecutive blanks
    // so a malformed file with kilobytes of empty siblings can't stall.
    const MAX_CONSECUTIVE_BLANKS: usize = 16;
    let mut start = lines.len();
    let mut comment_lines_taken = 0usize;
    let mut consecutive_blanks = 0usize;
    while start > 0 && comment_lines_taken < FALLBACK_DOC_MAX_LINES {
        let candidate = lines[start - 1];
        if !line_looks_comment_like(candidate) {
            break;
        }
        if candidate.trim().is_empty() {
            if consecutive_blanks >= MAX_CONSECUTIVE_BLANKS {
                break;
            }
            consecutive_blanks += 1;
        } else {
            consecutive_blanks = 0;
            comment_lines_taken += 1;
        }
        start -= 1;
    }
    // Strip leading blank lines from the selection so we don't return an
    // empty/whitespace-only `doc`.
    while start < lines.len() && lines[start].trim().is_empty() {
        start += 1;
    }
```

**Test to add:**

```rust
#[test]
fn fallback_walks_past_blank_gap_to_capture_earlier_comments() {
    let content = "\
-- header comment 1
-- header comment 2

-- header comment 3
CREATE TABLE x (id TEXT);
";
    let file = write_temp_file(content, "sql");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let chunk = chunks.iter().find(|c| c.name == "x").expect("x chunk");
    let doc = chunk.doc.as_deref().unwrap_or("");
    assert!(
        doc.contains("header comment 1"),
        "expected header comment 1 to survive blank gap, got: {doc:?}"
    );
    assert!(
        doc.contains("header comment 3"),
        "expected header comment 3 to be captured, got: {doc:?}"
    );
}
```

---

### A.3 — Algorithm: `BoundedScoreHeap::push` evicts the wrong tied element (P1 #5)

**File:** `src/search/scoring/candidate.rs:188-217`

**Why:** The min-heap stores `Reverse<(score, id)>` so `peek()` returns `(score min, id min)` — the *best* element among the lowest-scored. The eviction condition then evicts that "best at the boundary" instead of the "worst at the boundary" `(score min, id max)`. Under `rrf_fuse`'s HashMap-fed input (process-seed-randomized iteration), this produces non-deterministic top-K when scores tie at the boundary — exactly the AC-V1.25-1/AC-V1.25-2 class wave-1 was meant to close.

**Investigation note:** Re-read `src/search/scoring/candidate.rs:180-235` and the existing `test_bounded_heap_*` tests at lines 280-330+ before editing. Pin the doc comment "smaller id wins among ties" precisely.

**Fix shape:** Make the heap key reflect the actual eviction order. Wrap the id in `Reverse` too: store `Reverse<(OrderedFloat<f32>, Reverse<String>)>` (or equivalent for whatever the current id type is). Then `peek()` returns the worst element under the final sort order `(score desc, id asc)`: smallest score, *largest* id.

Eviction check becomes:
```rust
score > min_score || (score == min_score && id < min_id_largest)
```
where the peeked element's id is the largest id at the lowest score.

**Tests to add:**

```rust
#[test]
fn bounded_heap_deterministic_under_reverse_push_order() {
    let mut heap = BoundedScoreHeap::new(2);
    heap.push("c".to_string(), 0.5);
    heap.push("b".to_string(), 0.5);
    heap.push("a".to_string(), 0.5);
    let top = heap.into_sorted_vec();
    let names: Vec<_> = top.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(names, vec!["a", "b"], "expected smallest-id wins among ties");
}

#[test]
fn bounded_heap_deterministic_under_forward_push_order() {
    let mut heap = BoundedScoreHeap::new(2);
    heap.push("a".to_string(), 0.5);
    heap.push("b".to_string(), 0.5);
    heap.push("c".to_string(), 0.5);
    let top = heap.into_sorted_vec();
    let names: Vec<_> = top.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(names, vec!["a", "b"]);
}
```

---

### A.4 — Algorithm: `dot()` for neighbor search silently truncates on dim mismatch (P1 #6)

**File:** `src/cli/commands/search/neighbors.rs:67-70` (`fn dot`) + `:101` (call site)

**Why:** `Iterator::zip` truncates to the shorter input. After a partial `cqs model swap`, the index may contain a mix of 1024-dim and 768-dim embeddings. `dot()` silently computes over `min(len_a, len_b)` and produces well-defined-but-meaningless scores. Sister `cosine_similarity` correctly returns `None`.

**Current code:**

```rust
fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}
```

**Replacement code:**

```rust
/// Dot product for L2-normalized vectors. Returns `None` if the dimensions
/// disagree — indicates a partial reindex / mid-flight model swap and the
/// caller should skip the chunk rather than emit a meaningless score.
fn dot(a: &[f32], b: &[f32]) -> Option<f32> {
    if a.len() != b.len() {
        tracing::warn!(
            target_dim = a.len(),
            batch_dim = b.len(),
            "neighbors: embedding dim mismatch, skipping chunk (partial reindex?)"
        );
        return None;
    }
    Some(a.iter().zip(b.iter()).map(|(x, y)| x * y).sum())
}
```

**Call-site update** (around line 101 of the same file — re-read first to find the exact site):

```rust
// Before (assumed):
let sim = dot(target_slice, embedding.as_slice());
scored.push((id, sim));

// After:
if let Some(sim) = dot(target_slice, embedding.as_slice()) {
    scored.push((id, sim));
}
```

**Tests to add (in the existing `tests` mod inside neighbors.rs):**

```rust
#[test]
fn dot_returns_some_for_equal_dim() {
    let a = [1.0_f32, 0.0, 0.0];
    let b = [1.0_f32, 0.0, 0.0];
    assert_eq!(dot(&a, &b), Some(1.0));
}

#[test]
fn dot_returns_none_for_dim_mismatch() {
    let a = [1.0_f32, 0.0, 0.0];
    let b = [1.0_f32, 0.0];
    assert_eq!(dot(&a, &b), None);
}
```

---

## Group B — CLI JSON contract (independent files within `src/cli/commands/`)

### B.1 — `cqs --json eval` doesn't honor top-level `--json` (P1 #7)

**File:** `src/cli/commands/eval/mod.rs:95-99,112` (and verify the dispatch site at `src/cli/dispatch.rs:495`)

**Why:** Convention: top-level `--json` always wins. `cmd_eval` reads only `args.json`, ignores `ctx.cli.json`. Re-read the file before editing.

**Fix:** At the top of `cmd_eval`, resolve `let json = ctx.cli.json || args.json;` and use `json` everywhere downstream. Mirror the pattern at `src/cli/commands/io/notes.rs:524` (`if json || ctx.cli.json`) and `src/cli/commands/infra/model.rs:113` (`cli.json || *json`).

**Test to add (in `tests/eval_subcommand_test.rs`):**

```rust
#[test]
#[serial]
fn cli_top_level_json_wins_for_eval() {
    let dir = setup_eval_project();
    init_and_index(&dir);
    let qfile = write_temp_query_file(...); // tiny eval set
    let output = cqs()
        .args(["--json", "eval", qfile.path().to_str().unwrap()])
        .current_dir(dir.path())
        .output()
        .expect("eval should run");
    assert!(output.status.success());
    let parsed: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("envelope JSON");
    assert!(parsed["data"].is_object(), "got: {}", String::from_utf8_lossy(&output.stdout));
}
```

---

### B.2 — `cqs --json project` and `cqs --json cache` ignore top-level `--json` (P1 #8)

**Files:**
- `src/cli/dispatch.rs:69` (`cmd_cache(subcmd)`)
- `src/cli/dispatch.rs:147-149` (`cmd_project(subcmd, ...)`)
- `src/cli/commands/infra/cache_cmd.rs:35-46` (`cmd_cache`)
- `src/cli/commands/infra/project.rs:67-156` (`cmd_project`)

**Why:** Same precedence violation. `cmd_model` already does this correctly (`cli: &Cli` arg, `cli.json || *json`).

**Fix:** Change both `cmd_cache` and `cmd_project` signatures to accept `cli: &Cli`. Update dispatch sites to pass `&cli`. Inside each subcommand handler, resolve `let json = cli.json || *json;` and use that.

---

### B.3 — `cqs ping --json` (and `cqs eval --baseline`) emit text, not JSON envelope, on error (P1 #9)

**Files:**
- `src/cli/commands/infra/ping.rs:182-188` (Err arm)
- `src/cli/commands/eval/mod.rs:117-122` (regression-past-tolerance arm)

**Why:** PR #1038 envelope contract advertises `{data:null, error:{code, message}, version:1}` on failure, but these two error paths print `cqs: <msg>` to stderr and `process::exit(1)` instead. Activates the unwired `emit_json_error` helper for the first time.

**Fix (ping.rs:182-188 area):**

```rust
// Before (assumed):
Err(msg) => {
    eprintln!("cqs: {msg}");
    std::process::exit(1);
}

// After:
Err(msg) => {
    if json {
        crate::cli::json_envelope::emit_json_error(
            crate::cli::json_envelope::error_codes::IO_ERROR,
            &msg,
        )?;
    } else {
        eprintln!("cqs: {msg}");
    }
    std::process::exit(1);
}
```

**Same shape** for the eval regression-exit path. Use `error_codes::INTERNAL` for the regression case (it's not really an I/O error; consider if `INVALID_INPUT` fits better — re-read the failure context).

---

### B.4 — `notes add --sentiment NaN` poisons notes.toml (P1 #10)

**Files:**
- `src/cli/commands/io/notes.rs:69-70` (`Add::sentiment`)
- `src/cli/commands/io/notes.rs:86-87` (`Update::new_sentiment`)
- `src/cli/commands/infra/reference.rs:48-50` (`RefCommand::Add::weight`)

**Why:** API-V1.25-7 added `parse_finite_f32` for clap value_parsers to reject NaN/Infinity at parse. Three f32 flags were missed. `NaN.clamp(-1.0, 1.0) == NaN` → poisons notes.toml.

**Fix:** Add `value_parser = crate::cli::definitions::parse_finite_f32` to each of the three flag declarations. The parser is already `pub(crate)` at `src/cli/definitions.rs:121`.

```rust
// Example for Add::sentiment:
#[arg(long, value_parser = crate::cli::definitions::parse_finite_f32)]
pub sentiment: f32,
```

---

### B.5 — `cqs cache stats --json` returns `total_size_mb` as a string (P1 #11)

**File:** `src/cli/commands/infra/cache_cmd.rs:54-61`

**Why:** Field value is `format!("{:.1}", ...)` — a string. Every other JSON field is numeric. Programmatic consumers calling `obj["total_size_mb"] + 1` get a TypeError.

**Fix:** Drop `total_size_mb` and let consumers divide `total_size_bytes`, OR emit as `f64`:

```rust
"total_size_mb": stats.total_size_bytes as f64 / 1_048_576.0,
```

(no `format!`). Update any inline test in `cache_cmd.rs` to assert it's a number.

---

### B.6 — `cqs eval` `r1`/`r5`/`r20` field names break `r_at_K` convention (P1 #26)

**Files:**
- `src/cli/commands/eval/baseline.rs:28-33` (`KDelta { r1, r5, r20 }`)
- `tests/eval_baseline_test.rs` (assertions referencing the old field names)

**Why:** Sibling `EvalReport` (`src/cli/commands/eval/runner.rs:74-76,83-85`) uses `r_at_1 / r_at_5 / r_at_20`. Same command, two shapes confuses agents.

**Fix:** Rename `KDelta::{r1, r5, r20}` to `{r_at_1, r_at_5, r_at_20}` (or add `#[serde(rename = "r_at_1")]`). Update the test to match. Keep `R@1` / `R@5` / `R@20` strings in the `metric` value (those are human-display labels).

---

## Group C — Robustness panics (independent files)

### C.1 — `QueryCache::get` byte-slice query preview panics on multi-byte chars (P1 #12)

**File:** `src/cache.rs:1021-1028`

**Why:** `let preview_len = query.len().min(40); &query[..preview_len]` — if byte 40 lands inside a UTF-8 codepoint, the slice panics. The failure path is "DB error log" — turns a soft failure into hard process death.

**Fix:** Use `floor_char_boundary` (stable on MSRV 1.95):

```rust
// Before:
let preview_len = query.len().min(40);
tracing::warn!(error = %e, query_preview = %&query[..preview_len], "...");

// After:
let preview_len = query.floor_char_boundary(40);
tracing::warn!(error = %e, query_preview = %&query[..preview_len], "...");
```

Re-read the surrounding lines to keep the message text identical.

**Test to add (in cache.rs `mod tests`):**

```rust
#[test]
fn query_preview_does_not_panic_on_multibyte_query() {
    // Query that places a multi-byte CJK char straddling byte 40.
    let q = format!("{}café 注釈 emoji 🎉 more text past forty bytes", "x".repeat(35));
    assert!(q.len() > 40);
    // Just exercise the slicing — the warn log itself is harmless.
    let preview_len = q.floor_char_boundary(40);
    let _preview = &q[..preview_len]; // must not panic
}
```

---

### C.2 — `run_git_log` truncates git stderr by raw byte position (P1 #13)

**File:** `src/cli/commands/io/blame.rs:144-152`

**Why:** Same class as C.1. `&stderr[..MAX_STDERR_LEN]` may slice mid-codepoint when a non-ASCII path appears in git's error message.

**Fix:** Use `floor_char_boundary(MAX_STDERR_LEN)`:

```rust
let truncate_at = stderr.floor_char_boundary(MAX_STDERR_LEN);
format!("{}... (truncated)", &stderr[..truncate_at])
```

Re-read the surrounding lines for exact message text + variable names.

---

### C.3 — Path-traversal absolute-path check misses Windows UNC / `\\?\` paths (P1 #19)

**File:** `src/cli/display.rs:27` (the `if path_str.starts_with('/') || (path_str.len() >= 2 && path_str.as_bytes()[1] == b':')` guard)

**Why:** A tampered DB containing `chunk.file = \\\\evil-server\\share\\loot` slips past the guard. On Windows, that triggers an SMB connection — NTLM hash exfil via SMB relay attack.

**Fix:**

```rust
// Before:
if path_str.starts_with('/') || (path_str.len() >= 2 && path_str.as_bytes()[1] == b':') {
    anyhow::bail!("Absolute path blocked: {}", file.display());
}

// After:
if file.is_absolute() || path_str.starts_with("\\\\") || path_str.starts_with("//") {
    anyhow::bail!("Absolute path blocked: {}", file.display());
}
```

`Path::is_absolute` correctly recognizes drive-letter and (some) UNC on Windows. The two extra `starts_with` checks catch UNC consistently across platforms.

**Tests to add** (in `cli/display.rs::tests`):

```rust
#[test]
fn read_context_lines_rejects_unc_paths() {
    let p = Path::new("\\\\evil-server\\share\\loot");
    assert!(read_context_lines(p, 1, 1, 0).is_err());
}

#[test]
fn read_context_lines_rejects_extended_length_path() {
    let p = Path::new("\\\\?\\C:\\loot");
    assert!(read_context_lines(p, 1, 1, 0).is_err());
}
```

---

## Group D — Daemon / observability / data-safety / Windows

### D.1 — Envelope NaN sanitization parity: CLI `emit_json` + `cmd_chat` skip the retry-on-NaN that batch performs (P1 #1, #2)

**Files:**
- `src/cli/json_envelope.rs:121-125` (`emit_json`)
- `src/cli/json_envelope.rs:130+` (`emit_json_error`)
- `src/cli/chat.rs:225-231`
- `src/cli/batch/mod.rs:1030-1087` (existing `sanitize_json_floats` + `write_json_line`)

**Why:** Daemon/batch handle NaN gracefully via sanitize-and-retry. CLI `emit_json` and chat REPL just call `serde_json::to_string_pretty(&env)?` — `serde_json` returns Err for NaN/Infinity, which propagates as anyhow and stderr-prints. Same input, different observable output across surfaces.

**Fix shape:**

1. Move `sanitize_json_floats` from `src/cli/batch/mod.rs:1030-1051` to `src/cli/json_envelope.rs` (or expose it `pub(crate)`).
2. In `json_envelope.rs`, refactor `emit_json` and `emit_json_error` to follow the same try → on-Err sanitize-and-retry → on-Err emit envelope-error pattern that `write_json_line` uses. Concrete:

```rust
pub fn emit_json<T: Serialize>(value: &T) -> Result<()> {
    let env = Envelope::ok(value);
    let mut buf = serde_json::to_value(&env)?;
    let s = match serde_json::to_string_pretty(&buf) {
        Ok(s) => s,
        Err(_) => {
            sanitize_json_floats(&mut buf);
            serde_json::to_string_pretty(&buf)?
        }
    };
    println!("{s}");
    Ok(())
}
```

3. `cmd_chat` at chat.rs:225 — switch to a shared `format_envelope_to_string(&Value) -> Result<String>` helper that does the same try-sanitize-retry, then println the result. Define in `json_envelope.rs`.
4. Update `write_json_line` in `batch/mod.rs` to use the same helper / shared `sanitize_json_floats`.

**Tests to add (in `json_envelope.rs::tests`):**

```rust
#[test]
fn emit_json_sanitizes_nan_to_null() {
    use std::io::Write;
    let payload = serde_json::json!({"score": f64::NAN, "name": "x"});
    let mut buf = serde_json::to_value(Envelope::ok(payload)).unwrap();
    sanitize_json_floats(&mut buf);
    let s = serde_json::to_string_pretty(&buf).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&s).unwrap();
    assert!(parsed["data"]["score"].is_null());
    assert_eq!(parsed["data"]["name"], "x");
}

#[test]
fn emit_json_sanitizes_pos_and_neg_infinity() {
    let payload = serde_json::json!({"a": f64::INFINITY, "b": f64::NEG_INFINITY, "name": "x"});
    let mut buf = serde_json::to_value(Envelope::ok(payload)).unwrap();
    sanitize_json_floats(&mut buf);
    let s = serde_json::to_string_pretty(&buf).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&s).unwrap();
    assert!(parsed["data"]["a"].is_null());
    assert!(parsed["data"]["b"].is_null());
}
```

---

### D.2 — `dispatch_line` (CLI batch path) missing NUL-byte check the daemon socket loop enforces (P1 #14)

**File:** `src/cli/batch/mod.rs:466-510` (`dispatch_line`) vs `:1329-1343` (daemon socket loop)

**Why:** Same handlers downstream, divergent input validation. NUL bytes can bypass string processing in downstream consumers (RT-INJ-2).

**Fix:**

1. Extract a helper `fn reject_null_tokens(tokens: &[String]) -> Result<(), &'static str>` near `dispatch_line` in batch/mod.rs.
2. Call it from both `dispatch_line` (after `let tokens = match shell_words::split(...)` succeeds, before query_count.fetch_add) and the `cmd_batch` stdin loop (replacing the inline check at lines 1329-1343).
3. Both call sites emit `error_codes::INVALID_INPUT` with the same "Input contains null bytes" message.

---

### D.3 — Batch dispatch error envelope sites emit no `tracing::warn!` — daemon fails silently in journal (P1 #15)

**File:** `src/cli/batch/mod.rs:466-510, 1306-1401` (eight `write_envelope_error(...)` sites at lines 476, 502, 507, 1311, 1333, 1358, 1377, 1390)

**Why:** Operator on `journalctl -u cqs-watch` sees rising `error_count` from `cqs ping` but no clue what failed. Per the project memory: every error-fallback path needs `tracing::warn!` carrying the structured payload.

**Fix:** Add a `tracing::warn!` immediately before each `write_envelope_error` call carrying `code` and `error` fields. Concrete pattern:

```rust
// Before:
self.error_count.fetch_add(1, Ordering::Relaxed);
let _ = write_envelope_error(out, error_codes::INTERNAL, &format!("{e:#}"));

// After:
self.error_count.fetch_add(1, Ordering::Relaxed);
tracing::warn!(code = error_codes::INTERNAL, error = %format!("{e:#}"), "Batch dispatch error");
let _ = write_envelope_error(out, error_codes::INTERNAL, &format!("{e:#}"));
```

Apply to all 8 sites with the appropriate `code` constant per site.

**Bonus refactor:** Extract `fn report_dispatch_error(ctx, out, code, message, &e)` that does both the log and the write. Defer if the per-site arms are too heterogeneous.

---

### D.4 — `EmbeddingCache::open` swallows `set_permissions(0o600)` errors — asymmetric with QueryCache after SEC-V1.25-4 (P1 #18)

**File:** `src/cache.rs:131-147` (specifically the `let _ = std::fs::set_permissions(&db_file, perms.clone());` at line 144)

**Why:** SEC-V1.25-4 hardened QueryCache to log on chmod failure. EmbeddingCache was missed in the same wave. On a multi-user box where chmod fails, embedding cache files end up world-readable with zero log signal.

**Fix:** Mirror the QueryCache pattern verbatim:

```rust
// Before:
let _ = std::fs::set_permissions(&db_file, perms.clone());

// After:
if let Err(e) = std::fs::set_permissions(&db_file, perms.clone()) {
    tracing::warn!(
        path = %db_file.display(),
        error = %e,
        "Failed to set embedding cache permissions to 0o600"
    );
}
```

Re-read `src/cache.rs:979-1000` for the QueryCache version to mirror exactly.

---

### D.5 — `cqs read` existence check before traversal validation — daemon path-existence oracle (P1 #20)

**File:** `src/cli/commands/io/read.rs:24-29`

**Why:** Two distinguishable error messages = oracle. Daemon client can probe for `/home/other/.ssh/id_rsa` and learn host filesystem layout outside the project root.

**Fix:** Reorder the checks. Do `dunce::canonicalize` + `starts_with(root)` first, then check existence — both emit the same error message. Re-read the surrounding 30-50 lines to identify the canonical-path variables.

```rust
// After (sketch — adapt to actual variable names):
let canonical = dunce::canonicalize(&file_path).context("Invalid path")?;
let project_canonical = dunce::canonicalize(root).context("Invalid project root")?;
if !canonical.starts_with(&project_canonical) {
    anyhow::bail!("Invalid path");
}
if !file_path.exists() {
    anyhow::bail!("Invalid path");
}
```

Same fix may apply to `src/cli/commands/search/query.rs:864-875` — investigate, apply if same shape.

---

### D.6 — Daemon socket bind-then-chmod TOCTOU (P1 #21)

**File:** `src/cli/watch.rs:1206-1220`

**Why:** Between `UnixListener::bind` (creates socket honoring umask) and `set_permissions(0o600)`, the socket is world-creatable for ~ms. On `/tmp` fallback (`XDG_RUNTIME_DIR` unset) any local user can connect.

**Fix:** Set umask 0o077 immediately before bind, restore after:

```rust
#[cfg(unix)]
let prev_umask = unsafe { libc::umask(0o077) };
let listener = UnixListener::bind(&sock_path)?;
#[cfg(unix)]
unsafe { libc::umask(prev_umask); }
// chmod is now redundant but harmless — keep as belt-and-suspenders or remove
std::fs::set_permissions(&sock_path, std::fs::Permissions::from_mode(0o600))?;
```

Add a comment pointing at this finding so the next refactor doesn't drop the umask manipulation.

---

### D.7 — `aux_model::is_path_like` rejects Windows `C:\Models\splade` (P1 #22)

**File:** `src/aux_model.rs:117-119`

**Why:** Windows users can't pass an absolute path via `--reranker-model` etc. — gets routed through HF Hub fetch with `repo: Some("C:\\Models\\splade")`.

**Fix:**

```rust
// Before:
fn is_path_like(raw: &str) -> bool {
    raw.starts_with('/') || raw.starts_with("~/")
}

// After:
fn is_path_like(raw: &str) -> bool {
    raw.starts_with('/')
        || raw.starts_with("~/")
        || raw.starts_with("\\\\")
        || std::path::Path::new(raw).is_absolute()
}
```

**Tests to add:**

```rust
#[test]
fn is_path_like_accepts_windows_drive_letter() {
    assert!(is_path_like("C:\\Models\\splade"));
    assert!(is_path_like("D:/foo/bar"));
}

#[test]
fn is_path_like_accepts_unc_paths() {
    assert!(is_path_like("\\\\server\\share\\splade"));
}

#[test]
fn is_path_like_still_accepts_unix_absolute_and_tilde() {
    assert!(is_path_like("/usr/local/models/splade"));
    assert!(is_path_like("~/models/splade"));
}

#[test]
fn is_path_like_rejects_repo_id() {
    assert!(!is_path_like("mixedbread-ai/mxbai-edge-colbert-v0-32m"));
}
```

---

### D.8 — `find_python` error message hardcodes Linux/macOS install instructions (P1 #23)

**File:** `src/convert/mod.rs:77-80`

**Why:** Trivial UX fix; user-affecting for every Windows install.

**Fix:**

```rust
let install_hint = if cfg!(windows) {
    "Install Python from https://python.org/downloads/ or 'winget install Python.Python.3.12'"
} else if cfg!(target_os = "macos") {
    "macOS: 'brew install python'"
} else {
    "Linux: 'sudo apt install python3' (Debian/Ubuntu) or 'sudo dnf install python3' (Fedora)"
};
anyhow::bail!("Python not found in PATH. {install_hint}");
```

Re-read the exact bail message text first to keep wording consistent with the rest of the file.

---

### D.9 — `cli/dispatch.rs` notes path: `Option<CommandContext>` collapses store-open failure into clueless "Index not found" (P1 #24)

**File:** `src/cli/dispatch.rs:197-200`

**Why:** `.ok()` discards distinct failures (schema corruption, dim mismatch, permission denied) → user sees "Index not found" even when index.db exists.

**Fix:**

```rust
// Before:
let ctx = crate::cli::CommandContext::open_readonly(&cli).ok();

// After:
let ctx = match crate::cli::CommandContext::open_readonly(&cli) {
    Ok(c) => Some(c),
    Err(e) => {
        tracing::debug!(
            error = %e,
            "Notes: readonly store open failed; mutations will use write-only path"
        );
        None
    }
};
```

---

### D.10 — `dispatch_stats` (daemon) drops staleness fields the CLI populates (P1 #25)

**File:** `src/cli/batch/handlers/info.rs:246-252` vs `src/cli/commands/index/stats.rs:283-298`

**Why:** `cqs stats --json` via daemon emits `stale_files: null`/`missing_files: null`, but via `CQS_NO_DAEMON=1` emits actual counts. Agents auto-routed through daemon silently treat every project as fresh.

**Fix:** Mirror `cmd_stats:283-298` inside `dispatch_stats`. Enumerate files via `parser::Parser::new()?` like the CLI, call `ctx.store().count_stale_files(&file_set, &ctx.root)`, populate the two fields on the output. Re-read both files for exact field shape and `BatchContext` accessors.

---

## Group E — Data safety / migrations

### E.1 — Migration `UPDATE schema_version` silently does nothing if metadata row missing (P1 #16)

**File:** `src/store/migrations.rs:191-194`

**Why:** A DB without a `schema_version` row → migration runs all DDL → `UPDATE` affects zero rows → next open re-runs migrations → "duplicate column" / "table already exists" error.

**Fix:**

```rust
// Before:
sqlx::query("UPDATE metadata SET value = ?1 WHERE key = 'schema_version'")
    .bind(...)
    .execute(...)

// After:
sqlx::query("INSERT INTO metadata (key, value) VALUES ('schema_version', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value")
    .bind(...)
    .execute(...)
```

Re-read the exact bind variable names and `?` style first. Same pattern is already used at `migrations.rs:540-546, 600-605` for `splade_generation`.

---

### E.2 — `function_calls` rows leaked on every incremental delete path (P1 #17)

**Files:**
- `src/store/chunks/staleness.rs:69-151` (`prune_missing`)
- `src/store/chunks/crud.rs:427-449` (`delete_by_origin`)
- `src/store/chunks/crud.rs:539-605` (`delete_phantom_chunks`)
- Schema: `src/schema.sql:78-89` (`function_calls` table — verify FK absent)

**Why:** `function_calls` has no FK to `chunks` (stores `caller_name` strings). Three incremental delete paths leave function_calls rows alive forever → ghost callers in `cqs callers`/`callees`/`dead`.

**Fix:** In each of the three delete paths, add a parallel `DELETE FROM function_calls WHERE file = ?1` (for `delete_by_origin` and `delete_phantom_chunks` per-origin) or `DELETE FROM function_calls WHERE file IN (...)` (for `prune_missing`'s batched delete). Stay inside the existing transaction.

**Test to add (in `tests/store_calls_test.rs` or similar):**

```rust
#[test]
#[serial]
fn delete_by_origin_purges_function_calls() {
    let store = make_store();
    // Insert chunk + function_calls referencing src/foo.rs
    // ...
    store.delete_by_origin("src/foo.rs").unwrap();
    let count = store.count_function_calls_for_file("src/foo.rs").unwrap();
    assert_eq!(count, 0, "function_calls rows should be purged");
}
```

---

## Execution rules for the agent fixing these

- **Re-read the actual source at the cited line range before editing.** Files have been touched many times; lines may have drifted by a few lines.
- One commit per fix or per tightly-related cluster (e.g., A.1+A.2 together). Atomic per-fix beats atomic per-PR for triage.
- Use `cargo fmt` after each cluster.
- Run `cargo build --features gpu-index` after each cluster; never proceed if it doesn't compile.
- Run the tests added by each fix immediately, plus `cargo test --features gpu-index --lib parser::chunk` (or the relevant lib mod) for safety.
- When all P1 in a group are done, run the full lib-test suite once: `cargo test --features gpu-index --lib`.
- **Do NOT run `cargo test --features "gpu-index slow-tests"`** — those take 20+ min and aren't required for these fixes.
- **Skip P1 #15 if the surrounding code paths are too heterogeneous to share a helper** — leave the per-site `tracing::warn!` additions as the deliverable.
