## Resource Management

#### RM-V1.36-1: `truncate_incomplete_line` slurps entire training-data JSONL into memory
- **Difficulty:** easy
- **Location:** src/train_data/checkpoint.rs:56-77
- **Description:** `truncate_incomplete_line` calls `fs::read(path)` on the training-data output JSONL, then walks it in-memory just to find the last newline. Training-data JSONL files routinely run multi-GB (the doc-string says it's used for "crash recovery: partial JSONL lines"). Resume mode (`--resume`) on a 5-10 GB JSONL allocates a 5-10 GB Vec<u8> at startup, peak heap = 2× file size during the in-memory truncate path. Easy DoS surface and a real OOM on agent workstations with the v3.v2 fixture-scale corpora.
- **Suggested fix:** Open file, `seek(SeekFrom::End(-N))` for N=64 KiB, scan the tail buffer for the last `\n`, then `set_len(end_offset)` via `File::set_len`. Constant memory regardless of file size.

#### RM-V1.36-2: `pdf_to_markdown` captures unbounded subprocess stdout via `.output()`
- **Difficulty:** easy
- **Location:** src/convert/pdf.rs:20-25
- **Description:** `Command::new(python).arg(script).arg(path).output()` buffers the entire converter stdout in memory before returning. A 200 MB image-light PDF produces an order of magnitude more text than its bytes; a hostile PDF (or runaway pymupdf4llm) can produce arbitrary stdout that lands as a single `Vec<u8>` in our address space. There's no per-process cap and no per-call timeout — sibling subprocess code in `train_data/git.rs:167-211` already does the right thing with `Command::spawn` + `.take(max+1).read_to_end`.
- **Suggested fix:** Mirror the `train_data::git_diff_tree` pattern: spawn with `.stdout(Stdio::piped())`, wrap in `.take(max+1)`, kill child on overrun, surface a `ConvertError::OutputTooLarge`. Same env-overridable cap pattern (`CQS_PDF_MAX_BYTES`).

#### RM-V1.36-3: Position-IDs build allocates one throwaway Vec per row in the batch
- **Difficulty:** easy
- **Location:** src/embedder/mod.rs:1238-1241
- **Description:**
  ```
  let mut pos_data: Vec<i64> = Vec::with_capacity(texts.len() * max_len);
  for _ in 0..texts.len() {
      pos_data.extend((0..max_len as i64).collect::<Vec<i64>>());
  }
  ```
  The inner `.collect::<Vec<i64>>()` allocates a fresh Vec just to immediately consume it via `extend`. For a Qwen3-Embedding-4B batch of 128 × 2048 = 128 throwaway 16 KiB allocations per call. Also `texts.len() * max_len` is unchecked — `texts.len() * max_len` could overflow on 32-bit (cqs is 64-bit so practically fine, but the pattern is `saturating_mul` everywhere else).
- **Suggested fix:** `pos_data.extend(0..max_len as i64)` directly inside the loop (range iter is `Iterator` already). Use `saturating_mul` on the with_capacity arg for consistency with the rest of the codebase.

#### RM-V1.36-4: Watch-loop pre-bind probe `UnixStream::connect` has no timeout
- **Difficulty:** easy
- **Location:** src/cli/watch/mod.rs:540
- **Description:** Before binding the daemon socket, the watch loop does `UnixStream::connect(&sock_path)` to detect a peer daemon. No `Duration` is configured anywhere on this connect — std uses an OS default (Linux `SO_SNDTIMEO`/`SO_RCVTIMEO` unset → blocking). If the previous daemon is wedged in shutdown (mid-checkpoint, in TIME_WAIT analogue, or `accept` queue full), the connect blocks indefinitely and `cqs watch --serve` hangs at startup with no diagnostic. Compare `dispatch.rs:333-359` and `socket.rs:77-88` which BOTH set explicit timeouts.
- **Suggested fix:** Use `UnixStream::connect_timeout` (or set `set_nonblocking(true)` immediately after connect with a brief poll) and treat ETIMEDOUT as "no live peer, proceed with bind". Same 5 s default as `resolve_daemon_timeout_ms()`.

#### RM-V1.36-5: `Command::output()` in chm.rs / convert/mod.rs / train_data/mod.rs swallows whole subprocess output
- **Difficulty:** easy
- **Location:** src/convert/chm.rs:33-47, src/convert/mod.rs:65, src/train_data/mod.rs:528
- **Description:** Same shape as RM-V1.36-2: every `Command::output()` site materializes both stdout and stderr in memory unbounded. chm.rs nullifies stdout but pipes stderr unbounded; convert/mod.rs's binary-existence probe pipes both. A misbehaving 7z that endlessly emits errors → unbounded `Vec<u8>` per invocation. Fewer bytes per call than the PDF case, but same shape and no per-process cap.
- **Suggested fix:** For probes (existence checks) use `.stderr(Stdio::null())`. For real conversions, use the spawn+take pattern from `git.rs`. Single `bounded_output(cmd, max)` helper would unify all five sites.

#### RM-V1.36-6: `add_reference_to_config` / similar config-write paths re-read full file under lock
- **Difficulty:** easy
- **Location:** src/config.rs:820-867 (and analogous remove path ~line 956)
- **Description:** The atomic config-update code path opens the file, takes an exclusive flock, then reads-modifies-writes. The size cap (RM-V1.33-1) is in place, but the read+TOML parse+full-rewrite cycle materializes 3 copies of the config in memory simultaneously (raw `String` content, parsed `toml::Table`, serialized output). With `MAX_CONFIG_SIZE=1 MiB` (default) this is fine; if an operator overrides via env it's cubic in the cap. More importantly, the lock is held across blocking reqwest calls in callers (validation), pinning the file for tens of seconds.
- **Suggested fix:** Read+parse under the flock, drop the flock before any network I/O, re-acquire flock for the final atomic-write phase only. Standard "minimize critical section" refactor — affects `add_reference_to_config` and `remove_reference_from_config` symmetrically.

#### RM-V1.36-7: `BufReader::lines()` in display.rs allocates per-line with no per-line cap
- **Difficulty:** easy
- **Location:** src/cli/display.rs:204-208, src/cli/display.rs:635-639
- **Description:** Both `read_window_lines` paths use `BufReader::new(f).lines().take(limit)`. `BufRead::lines()` allocates a fresh `String` per line via `read_line` — a single pathological source file with a 500 MB line (minified JS bundle, generated lockfile, single-line WASM disassembly) becomes a 500 MB heap allocation even when the caller only wants `limit=20` lines around `line_start`. The `take(limit)` short-circuits the iterator after N lines but doesn't bound the per-line `String` size.
- **Suggested fix:** Use `read_until(b'\n', &mut buf)` with a per-line `take` cap (e.g. 1 MiB) or check `buf.len()` and skip lines exceeding the cap, replacing them with a `[line truncated]` marker. Same pattern other defensive readers already use.

#### RM-V1.36-8: Daemon accept loop polls with 100 ms blocking sleep — wakeup latency on shutdown
- **Difficulty:** easy
- **Location:** src/cli/watch/daemon.rs:212-213
- **Description:** `Err(WouldBlock) => std::thread::sleep(Duration::from_millis(100))` — the accept loop sleeps a flat 100 ms when no client is waiting. On SIGTERM/Ctrl-C the `daemon_should_exit` check at top of loop only fires once per accept tick, so the daemon exit latency is **up to 100 ms** + the time for whatever's currently in-flight to drain. Not a real DoS, but it accumulates: every accept-loop iteration that lands in WouldBlock costs a thread-park syscall. With the 60-second idle-sweep tick window, that's 600 wakeups/min on an otherwise-idle daemon — 600 wasted scheduler trips/min × N daemon processes on the workstation.
- **Suggested fix:** `epoll_wait` / `mio` / `poll` on the listener fd with `daemon_should_exit` checked between events. Or at minimum, raise the sleep to 500 ms — agents poll for fresh on the order of seconds, no need for 10 Hz wakeups when idle.

#### RM-V1.36-9: HNSW build holds full `id_map` Vec<String> in memory at peak
- **Difficulty:** medium
- **Location:** src/hnsw/build.rs:169-209
- **Description:** `build_with_dim_streaming` pre-allocates `Vec::with_capacity(capacity)` for `id_map` where `capacity` is the chunk count fetched from the store. At ~80 chars/chunk-id × 1M chunks = 80 MB just for the id strings — on top of the HNSW graph itself. The streaming-batches design correctly avoids holding all embeddings simultaneously, but `id_map` is the inverse: every entry is retained for the whole build. For very large corpora (the SPLADE-Code 0.6B / Qwen3-4B target), this is the largest single allocation outside the HNSW graph.
- **Suggested fix:** `Vec<Arc<str>>` halves the per-entry overhead vs `Vec<String>` (no separate len/capacity per entry). Or compress: `Vec<u32>` indices into a deduplicated string-arena. Real fix (multi-PR) is to write the id_map directly to disk as it's built and mmap on load — same shape as the embeddings_arena work.

#### RM-V1.36-10: `Vec::with_capacity(chunk_count * dim)` in CAGRA build is unchecked multiplication
- **Difficulty:** easy
- **Location:** src/cagra.rs:746-747
- **Description:** `Vec::with_capacity(chunk_count * dim)` directly multiplies — `chunk_count` is store-controlled (could be in the millions) and `dim` is model-controlled (could be 4096 for Qwen3-4B). On 64-bit the practical overflow risk is gone, but the *allocation* is unchecked: 1M chunks × 4096 dim × 4 B/f32 = 16 GiB. The `cagra_max_bytes` check at line 736-744 happens **before** these `with_capacity` calls, so OOM-via-allocator is gated. But a corrupt store reporting `chunk_count = usize::MAX` slipping through `embedding_count()` would still hit `Vec::with_capacity(usize::MAX)` → panic. Belt-and-suspenders: the same `try_into::<usize>` + sanity-bound pattern as `splade/index.rs:653-664` should apply here.
- **Suggested fix:** Add an upper bound assertion (`chunk_count <= 1<<28` say, matching the SPLADE pattern) before the `with_capacity` calls. Or prefer `Vec::new()` + `extend_from_slice` per batch (the loop already streams batches, the up-front `with_capacity` is the only reason this matters).

DONE
