# screw-mcp — Design

**Status:** rough draft, audit pending
**Date:** 2026-05-08
**Location:** `tools/screw-mcp/` — workspace member of cqs, excluded from the published `cqs` crate (precedent: `tools/` is in `cqs/Cargo.toml`'s `exclude = [...]`)
**Provenance:** sketched in a sidebar to the v1.39.2 ship session. The user has been making screw tapes by hand in Sound Forge for years and wanted a smaller-surface tool the agent harness could drive.

---

## What this is, in plain language

A **screw tape** is a slowed-down, chopped-up hip-hop mix in the style pioneered by DJ Screw (Houston, 1990s) — source tracks pitched and slowed in lockstep (via turntable varispeed, ~25-33% slow), with rapid live-cut "chops" of bars and phrases, and flange/echo sweeps on transitions. The vibe lives in the period-doubling artifact a turntable produces: vocals get a slowed/deeper timbre, drums become warped. Modern formant-preserving time-stretchers explicitly do not produce this sound; the workflow needs varispeed (pure resampling), not pitch-preserving stretch.

**screw-mcp** is an offline audio-edit MCP server, written in Rust, that exposes the small set of primitives needed to compose screw tapes from a playlist. Its primary user is an agent (Claude over the Anthropic MCP protocol) with a list of source tracks; the agent calls tools to slow, select bars, chop, flange, and concatenate, then renders out a single MP3.

This is not a Sound Forge clone, not a DAW, not a DJ tool, not a live performance system. It is a small, focused, agent-driven offline editor.

## Why this exists

Sound Forge does the job but is far more tool than the workflow needs (~5% of its surface is in use), runs only on Windows, has no MCP surface, and demands per-track human attention that defeats the point of having an agent with a playlist. A weekend in Rust on top of `symphonia` + `aubio` + the Anthropic MCP SDK gets us a focused tool with the right surface and at least the option of automation.

## Glossary

| Term | Meaning |
|---|---|
| **Varispeed** | Slowing (or speeding) audio by changing the playback sample rate — pitch and tempo move in lockstep. The screw-tape sound. **Distinct from time-stretch**, which preserves pitch by phase-vocoder or granular methods. |
| **Time-stretch** | Tempo change with pitch held constant (e.g. élastique, RubberBand). Wrong for screw tapes; produces the "slowed YouTube video" vibe instead. |
| **Chop** | A short stutter or repeat-cut of a bar, half-bar, or quarter-bar segment. Live, rhythmic. Defines the genre. |
| **Bar / Beat** | Musical units. A 4/4 hip-hop bar at 92 BPM is ~2.61 seconds. |
| **BarFraction** | A tempo-synced fraction (1/4, 1/8, 1/16) used to specify chop slice widths and tempo-synced FX rates. |
| **Region** | A named, content-addressed slice of audio with a sequence of edit operations applied. Immutable; transformations produce new regions. |
| **Content-addressed handle** | A `RegionId` is the blake3 hash of `(source_track_id, range, ops_applied)`. Two paths to the same region give the same handle. Caching and retries become free. |
| **MCP** | Model Context Protocol — Anthropic's protocol for exposing tools to an LLM agent. The Rust SDK is `rmcp`. |
| **Subscription-service ingest** | Out of scope for this server. If an upstream tool wants to capture from Spotify/Apple/Tidal, it does so via system-audio loopback (PipeWire / WASAPI / BlackHole) and writes MP3 files; screw-mcp only ever sees files. |

## Goal

Agent-driven, offline audio editor specialized for composing screw tapes from a playlist. Inputs: audio files. Output: a single rendered MP3 mix file per call to `compose_tape` / `render`.

**The build is a Rust CLI binary first** (`screw-tape`) with the MCP wrapper bolted on as v1.5 — but the CLI is designed to be MCP-compliant from day one (see "CLI ↔ MCP discipline contract" under Build phasing). Same operations, two transports; the JSON tool schemas are the source of truth for both surfaces.

The defining constraint: the surface should be small enough that a competently prompted agent produces listenable output without prompt-engineering acrobatics. ~6 high-level "stylistic" tools are the agent's default reach; primitives exist as escape hatches when the agent wants something specific.

## Non-goals

- Live performance / DJ workflow (deferred to v2; see Build phasing)
- VST / AU plugin hosting
- General-purpose audio editor (Audacity, Reaper, Sound Forge own this)
- Multitrack mixing beyond crossfade-style overlays
- Any UI surface
- Stream-source ingest (Spotify, Apple, Tidal). Pre-capture to MP3 via a separate tool.
- Subscription DRM circumvention. Loopback capture of the user's own playback session is the legal path.

## Architecture

Single Rust binary (`screw-tape`) for v1; an `rmcp`-based MCP server crate (`screw-tape-mcp`) bolted on at v1.5 that calls the same library entry points. Workspace state in-memory with optional disk-backed cache for content-addressed regions. v1 is offline batch — no audio-output thread, no driver layer, no real-time DSP. v2 (Build phasing below) layers a live playback engine on top of the same offline core.

```
┌────────────────────────────────────────────────────────────────────┐
│ screw-tape-core (Rust library; the actual logic)                   │
│                                                                    │
│  Ingest:  symphonia decode → aubio tempo/beat/onset                │
│           → AnalyzedTrack stored in workspace                      │
│           [optional Python sidecar at ingest for                   │
│            section labelling — writes JSON, Rust reads]            │
│                                                                    │
│  Workspace: HashMap<TrackId, AnalyzedTrack>                        │
│             HashMap<RegionId, RegionDef>                           │
│             content-addressed (blake3); retries free               │
│                                                                    │
│  Edit ops:  immutable region transformations.                      │
│             handle(s) + args → new handle.                         │
│             ops_applied: Vec<String> tracks lineage.               │
│                                                                    │
│  Render:   resolve region tree → streaming PCM source              │
│            → FFmpeg subprocess for encode → file                   │
└────────────────────────────────────────────────────────────────────┘
            ▲                                          ▲
            │                                          │
   ┌────────┴──────────┐                ┌──────────────┴──────────┐
   │ screw-tape (CLI)  │                │ screw-tape-mcp          │
   │ v1, ships first   │                │ v1.5, mechanical bolt-on │
   │ clap subcommands  │                │ rmcp tool handlers      │
   │ --json mode IS    │                │ shares all schemas with │
   │ MCP response      │                │ the CLI binary          │
   │ shape             │                │                          │
   └───────────────────┘                └─────────────────────────┘
```

Both surfaces use the same JSON tool schemas (defined as Rust types with `schemars` derive). CLI subcommands derive their clap definitions from the schema. MCP tool handlers wrap the schema-typed library entry points. **No duplicated type definitions, no hand-maintained parallel surfaces.**

External dependencies:
- `aubio` (C lib via `aubio-rs`) for tempo + beat + onset detection
- `ffmpeg` subprocess for encoders; do not ship encoder libs in-tree
- Optional `python3` sidecar at ingest for section labelling (madmom). Degrades gracefully to bar-only addressing if absent. Each sidecar gets its own pinned `pyproject.toml` + `uv.lock` so its dep tree doesn't bleed.

## Type model

Newtypes for every unit. Most audio bugs are unit-confusion bugs and the type system is the cheap defense.

```rust
pub struct Bar(u32);              // bar index from 0
pub struct Beat(u32);             // absolute beat index
pub struct Sample(u64);           // sample offset; u64 to not wrap on long mixes
pub struct Tempo(f32);            // BPM
pub struct SampleRate(u32);       // usually 48000
pub struct BarFraction(u8, u8);   // 1/8 = (1, 8); for chop widths and FX rates

pub struct TrackId([u8; 32]);     // blake3 of decoded source bytes
pub struct RegionId([u8; 32]);    // blake3 of (track_id, range, ops_applied)

pub struct AnalyzedTrack {
    id: TrackId,
    tempo: Tempo,
    bar_count: u32,
    duration: Sample,
    sample_rate: SampleRate,
    key: Option<String>,            // aubio_pitch best-effort
    sections: Option<Vec<Section>>, // None unless Python sidecar ran
    pcm: Arc<[f32]>,                // interleaved stereo; Arc shared across regions
}

pub struct RegionDef {
    id: RegionId,
    source: SourceRef,        // Track(TrackId) | Composed(Vec<RegionId>)
    span: BarRange,
    ops: Vec<EditOp>,         // applied in order at render time
    duration: Sample,         // computed
    ops_applied: Vec<String>, // human-readable for the agent
}
```

Conversions cross unit boundaries in exactly one place each (`bar_to_sample`, `beat_to_sample`, etc.) with their own focused tests.

## Tool surface (layered)

### High-level "stylistic" tools (agent's default reach)

```
ingest_track(path: PathBuf) -> TrackHandle
ingest_playlist(paths: Vec<PathBuf>) -> Vec<TrackHandle>

screwed_section(
    track: TrackId,
    start_bar: Bar, end_bar: Bar,
    slow_ratio: f32 = 0.75,
    chop_pattern: ChopPattern = ChopPattern::Stutter(BarFraction(1, 8)),
    flange_amount: f32 = 0.3,
) -> RegionHandle

chopped_intro(track: TrackId, bars: u32 = 8) -> RegionHandle
crossfade_to(a: RegionId, b: RegionId, overlap_bars: u32 = 2) -> RegionHandle
compose_tape(regions: Vec<RegionId>, output: PathBuf) -> RenderResult
```

### Low-level primitives (escape hatches)

```
slow_down(track: TrackId, ratio: f32) -> TrackHandle
select_bars(track: TrackId, start: Bar, end: Bar) -> RegionHandle
chop_region(region: RegionId, pattern: ChopPattern) -> RegionHandle
loop_region(region: RegionId, count: u32) -> RegionHandle
reverse_region(region: RegionId) -> RegionHandle
fade(region: RegionId, in_bars: u32, out_bars: u32) -> RegionHandle

flange(region: RegionId, rate: BarFraction, depth: f32, feedback: f32, mix: f32) -> RegionHandle
echo(region: RegionId, time: BarFraction, feedback: f32, mix: f32) -> RegionHandle
lowpass(region: RegionId, cutoff_hz: f32, resonance: f32) -> RegionHandle

concat(regions: Vec<RegionId>) -> RegionHandle
render(region: RegionId, output: PathBuf, format: AudioFormat) -> RenderResult
```

### Returns are rich

```
TrackHandle  { id, tempo, bar_count, duration_secs, sample_rate, key, sections }
RegionHandle { id, source_track_id, start_bar, end_bar, duration_secs, ops_applied }
RenderResult { path, duration_secs, lufs, peak_db, ok }
```

`ops_applied` is load-bearing: the agent inspects it to avoid double-applying effects, and the harness uses it for cache lookups.

## Determinism

Same input bytes + same args → byte-identical output. Non-negotiable because content-addressed caching breaks otherwise and tests can't pin specific sample values.

Concretely:
- Pin resampler config (libsoxr `VHQ`)
- No dither at intermediate stages; if we dither at output it's TPDF with a fixed seed
- Single-threaded analysis; no parallelization across tracks in a non-deterministic order
- FFmpeg invocation with explicit `-c:a libmp3lame -q:a 0` (or pinned equivalent)

Live playback (v2) does NOT preserve byte-identity to offline render — the audio device's resampler and dither break it. That's expected. Tests have to know which mode they're in. The internal DSP graph upstream of the output layer remains deterministic.

## Disciplines (carried over from cqs conventions)

These mirror the disciplines codified in `CLAUDE.md` and `~/.claude/projects/-mnt-c-Projects-cqs/memory/MEMORY.md`. Linked here so a cold reader knows the source.

- **Newtype every unit.** `Bar` and `Sample` are not interchangeable.
- **`thiserror` per module** (`IngestError`, `EditError`, `RenderError`); `anyhow` only at the MCP tool boundary.
- **`tracing::info_span!` on every public function**, with structured fields, not log strings.
- **Zero allocations in the inner audio loop.** Preallocated PCM buffers, fixed-size delay lines, no `Vec::push` per sample. Load-bearing for v2 live playback even if v1 doesn't strictly need it.
- **Streaming region readers from day one.** A region exposes a `samples()` iterator (or chunked block reader); the offline render collects to PCM, the v2 live playback streams the same iterator to a sound card. Materializing whole regions to `Vec<f32>` is forbidden; that would force a v2 rewrite.
- **No `unwrap()` outside tests.** No `unwrap_or_default()` swallowing real errors.
- **Tests assert specific sample values**, not "non-empty". Bar arithmetic gets pinned numbers per `(BPM, sample_rate, bar)` triple.
- **`schemars`-derived JSON schemas** — the Rust type is the source of truth.
- **Comments explain WHY only.** Per CLAUDE.md, comments document non-obvious constraints, upstream-bug workarounds, contracts the type system can't express. No comments restating the function name.
- **No "framework" patterns** until a second instance demands them. Three flanger functions before an `Effect` trait.
- **Scope discipline.** This is the offline-edit MCP server (v1) + live playback layer (v2). Anything else is its own justified-on-its-own-merit project.

## Build phasing

### v1 — CLI-first build with MCP-discipline contract (3-4 weeks solo)

**The build target for v1 is a Rust CLI binary** (`screw-tape` or similar), not an MCP server. The MCP wrapper is bolted on as v1.5 once the CLI surface settles in real use. **But the CLI is designed to be MCP-compliant from day one** so the wrapper is mechanical mapping rather than a redesign.

This pattern was learned from cqs's own evolution: cqs went MCP-first → CLI-only at v0.10.0, and the CLI surface evolved through real use without MCP discipline. Re-adding MCP today would mean restructuring positional args + flag names to match MCP-tool-call shape. Building screw-tape CLI-first while keeping MCP discipline avoids that refactor.

#### CLI ↔ MCP discipline contract

Every CLI surface decision is constrained by these ten rules. Treat them as compile-time invariants for the build; reject any subcommand or flag that violates one.

1. **One subcommand = one MCP tool.** Strict 1:1 isomorphism. No subcommand maps to multiple MCP tools; no MCP tool spans multiple subcommands.
2. **Named long flags only.** No positional args except the single primary subject (e.g., the input file path). MCP params are named; positional CLI args don't translate.
3. **Every flag has a typed clap `value_parser`.** Numeric ranges, string enums, file path validation. Maps directly to JSON Schema constraints.
4. **`--json` returns the exact structured data the MCP tool would.** Same field names, same types. Text mode can be human-friendly; JSON mode IS the MCP response shape.
5. **State is content-addressed, never implicit.** No "current selection" or "active track" between calls. Track / Region handles returned in every relevant response. Matches MCP's stateless-tool-with-workspace-handles model.
6. **Errors are structured JSON with `code` + `message`** even on the CLI path. Per the SNR-restoration design (success → bare data on stdout; failure → structured JSON on stderr + non-zero exit), CLI failure shape IS the MCP error shape.
7. **No interactive prompts, no paging, no readline, no terminal detection.** Anything an agent can't drive over a one-shot tool call is forbidden.
8. **`help =` strings written for an agent.** Short, specific about input/output shape. Those strings become the MCP tool descriptions verbatim when the wrapper materializes.
9. **Idempotent, side-effect-free where possible.** Same inputs → same output. Side effects (file writes) only on explicit output-path flags. No ambient working-set state.
10. **One workspace per process.** Don't accumulate cross-invocation state. CLI batch mode (chained subcommands sharing a workspace) is just an MCP server's request-handling loop with a different transport.

**Source of truth: the JSON tool schema.** Design the MCP-tool args + result schema FIRST per subcommand. Derive the clap subcommand from the schema, not the other way around. The CLI is one rendering of the schema; the MCP wrapper is another. If a flag doesn't have a clean schema mapping, it doesn't ship.

#### Phasing

| Phase | Scope | Time |
|---|---|---|
| **0. Schema + skeleton** | For each v1 subcommand, define the MCP tool's args + result schema (Rust types with `schemars` derive). clap subcommand definitions auto-derive from / match the schema. CLI binary `screw-tape` boots, ingests via `aubio-rs`, returns `TrackHandle` JSON to stdout under `--json`. | week 1 |
| **1. Edit primitives** | `screw-tape select-bars`, `slow`, `loop`, `chop`, `concat`, `render`. Streaming region readers + content-addressed cache wired. Each subcommand passes the discipline contract. | week 2 |
| **2. FX** | `screw-tape flange`, `echo`, `lowpass` — all `BarFraction`-synced. ~150 lines DSP each, no third-party FX libs. | mid week 3 |
| **3. Stylistic tools** | `screw-tape screwed-section`, `chopped-intro`, `crossfade-to`, `compose-tape`. Composed from primitives via subprocess chaining or in-process for performance — both shapes preserve the schema-driven contract. | end week 3 |
| **4. Polish** | Error variants pruned, tracing fields filled, tests on bar arithmetic + sample-accuracy seams, README, agent prompt examples. Real-use exercise of the CLI for ~1 week to confirm the surface settles. | week 4 |

#### v1.5 — MCP wrapper (after v1 ships and the CLI surface stabilizes)

| Phase | Scope | Time |
|---|---|---|
| **4.5. MCP wrapper crate** | Add `screw-tape-mcp` crate to the workspace. For each CLI subcommand, generate an `rmcp` tool handler that takes the schema-derived JSON args, invokes the CLI's library entry point (NOT a subprocess shell-out), returns the schema-derived JSON result. Mostly mechanical from the discipline-contract groundwork. | 3-5 days |

The wrapper is a separate crate so the CLI binary stays usable without MCP, and the MCP server's deps (`rmcp`, the MCP runtime) stay isolated from the CLI consumer's dep tree. **No CLI restructuring required at v1.5.** If the discipline contract held during v1, the wrapper is generation, not redesign.

### v2 — Live playback layer (~2 weeks more, separately scheduled)

Adds a real-time playback engine on top of the offline core. Justified separately when v1 is in production use and the agent harness exercises live composition. The disciplines above (no-alloc DSP, streaming readers, deterministic order) are what make v2 cheap — about 1.5-2 weeks instead of a rewrite.

| Phase | Scope | Time |
|---|---|---|
| **5. Audio I/O** | `cpal` output stream, driver-agnostic glue (PipeWire / WASAPI / CoreAudio). | 3-5 days |
| **6. Real-time bus** | Lock-free SPSC ringbuf (`rtrb`) between MCP control thread and audio thread. | 2 days |
| **7. Bar-clock** | Audio-thread bar/beat clock for quantized event triggers. | 2-3 days |
| **8. Live MCP tools** | `play(region)`, `stop()`, `set_loop(region)`, `chop_now(pattern)`, `swap_fx(region, new_fx)`. | 3-4 days |

## Driver-model layer (out of v1 scope; informs CLI design)

The screw-tape CLI is the executor. An LLM-based agent harness (`screw-tape-dj`) sits above it, picks tracks and ops, and orchestrates the CLI subcommands. The harness itself is a separate project planned after v1 ships and the CLI surface settles in real use. Notes here exist because the choice of driver model imposes one v1 design implication.

### Driver-model trade space (as of 2026-05-08)

| Driver | VRAM | Active params | Hears audio? | Tool-calling | Context |
|---|---|---|---|---|---|
| Gemma 3 31B (current vLLM stock) | ~22GB Q5 | 31B | ❌ | strong | 128K |
| Gemma 4 26B-A4B (MoE, 8-of-128) | ~50GB int8 | 3.8B | ❌ | TBD; should be strong | 256K |
| **Gemma 4 E4B** | **~9GB bf16** | **4.5B effective** | **✅ (text + image + audio)** | **TBD** | **128K** |
| Gemma 4 E2B | ~5GB bf16 | 2.3B effective | ✅ | TBD | 128K |
| Mixtral 8x7B | ~26GB Q4 | 12.9B | ❌ | strong | 32K |

The most interesting candidate is **Gemma 4 E4B** because it accepts audio input alongside text. Two structural reasons this matters more than the parameter-count math suggests:

1. **Implicit DJ priors from audio pre-training.** A multimodal model trained on substantial audio data has heard millions of musical transitions, beat-matches, EQ moves, mixing patterns. Its "what makes a good transition feel right" knowledge lives in audio space, not just text space. A text-only model can read about screw tapes; an audio-multimodal model has *heard* them. For a primarily-aesthetic task like chop placement, that's a categorical shift, not a marginal one.

2. **E2B/E4B are not shrunken siblings — the "E" stands for "effective" parameters.** Per the official Gemma 4 model card: the model has a higher raw-parameter count (5.1B for E2B, 8B for E4B) but uses Per-Layer Embeddings (PLE) to reduce the inference-time footprint to the "effective" 2.3B / 4.5B figures. The trick: keep capacity for training, shed memory for deployment. Separately, these variants are marketed for edge / on-device use (phones, laptops, embedded), but the letter doesn't stand for "edge" — the letter refers to the parameter-counting mechanism. The implication is the same direction either way: per-effective-parameter capability is engineered above the generic-small-model curve, so the 4.5B-effective figure should not be interpreted as "much weaker than 26-31B" for the multimodal tasks the family was optimized for.

Together: an audio-multimodal driver that has implicit DJ priors AND fits comfortably in 9GB VRAM is a substantially better fit for the screw-tape DJ task than a larger text-only model. The audio-blindness limit dissolves; the cultural-priors lane closes most of its gap because of device-class engineering.

### v1 implication for the CLI

For an audio-multimodal driver to operate, the harness must be able to extract short PCM clips of regions from screw-tape's workspace. **One v1 CLI requirement:** `render` must support raw-PCM or short-WAV output, not just MP3. Two paths:

1. **Extend `render`** with `--format pcm | wav | mp3` (default mp3 for `compose-tape`-style end output; pcm/wav for clip extraction).
2. **Add a dedicated `clip` subcommand** that writes raw-PCM directly without the encode-to-file overhead. Faster for an audio-feeding-loop where the harness extracts dozens of candidate clips per decision.

**Default for v1: extend `render`.** The encode overhead for a 1-bar PCM dump is ~10ms via FFmpeg subprocess — invisible against the harness's per-decision LLM forward pass (~hundreds of ms even on E4B). Add `clip` later only if profiling shows the inner loop matters.

### Phasing (when the build begins, after v1 CLI ships)

1. **Phase 1: Gemma 3 31B as baseline.** Already in vLLM inventory. Build a working `screw-tape-dj` harness on top, observe what the bottleneck actually is. Empirical baseline.
2. **Phase 2: Side-by-side E4B vs 26B-A4B vs 31B-baseline.** Three drivers, same harness, same playlist. Listen to outputs. The audio-aware variant should win on chop selection; the larger text variants should win on track-pairing breadth. Likely outcome: E4B wins overall by enough margin that it becomes default.
3. **Phase 3 (maybe never): ensemble.** E4B for audio-conditioned section selection, 26B-A4B for cross-track pairing decisions, screw-tape CLI as executor. Two models on one A6000 at int8; coordinate via the harness.

### What's deliberately not solved here

- Building `screw-tape-dj` itself (separate v2-ish project)
- Selecting a default driver model in v1 (the CLI is driver-agnostic)
- Wiring vLLM (or any inference server) into the v1 build
- Audio-similarity embedding (vs audio-input-to-multimodal-model — different mechanism; not relevant here)

The driver-model layer is documented here only to lock in the v1 CLI design implication: **`render` exposes PCM/WAV output so audio-multimodal drivers are eventually feasible.** Everything else is post-v1.

## Decision log

### Settled by this document (do not re-litigate)

- **Language: Rust.** Same toolchain as cqs; the cqs codebase's disciplines transfer directly.
- **Repo location: `tools/screw-mcp/` as a workspace member of cqs**, not a separate repo. Already-excluded from the published `cqs` crate via `exclude = ["tools/", ...]`. No external crates.io publication.
- **CLI-first build with MCP-discipline contract.** v1 ships as a `screw-tape` CLI binary; the MCP wrapper is a separate `screw-tape-mcp` crate bolted on at v1.5. The CLI is designed to be MCP-compliant from day one (ten-rule contract above). Pattern learned from cqs's own MCP-first → CLI-only evolution at v0.10.0: building MCP-first commits to schemas before operations are exercised; building CLI-first lets operations evolve while keeping schema discipline. **Best of both modes.**
- **Schema is the source of truth.** Per-subcommand JSON tool schemas (Rust types with `schemars` derive) drive both clap subcommand definitions AND the v1.5 MCP wrapper. No parallel surfaces, no hand-maintained type duplication.
- **Python sidecar pattern, not in-process.** Subprocess JSON IPC; sidecars run at ingest time, not in the request hot path. Each sidecar has its own pinned `uv.lock`.
- **Offline-first; live deferred to v2.** v1 is a clean 3-4 week CLI build. v2 layers a live playback engine on top in ~2 weeks once v1 is in real use. Disciplines that enable v2 (no-alloc DSP, streaming readers, deterministic order) are committed to from day one.
- **Tool surface is layered**: high-level "stylistic" tools as the agent's default reach; low-level primitives as escape hatches. Both surfaces ship together.
- **Content-addressed handles** (blake3) for tracks and regions; caching and retries are free.
- **Determinism is non-negotiable** for offline render. v2 live playback acknowledges device-side non-determinism and tests handle the distinction.
- **No GUI, no plugin host, no DAW features, no live capture in v1.**
- **Subscription-service ingest is out of scope.** Pre-capture to file via a separate tool; screw-tape only sees files.

### Open — to settle in audit

1. **Section detection.** Ship without (bar-only addressing) or include the Python sidecar (madmom) at ingest? Without is leaner; with is more natural for the agent's prompts ("loop the chorus" beats "loop bars 65-80"). Default position: ship without; add sidecar in v1.5 if agent output feels structurally tone-deaf.
2. **Output formats.** MP3 only for v1, or WAV + FLAC + AAC + MP3? More formats = more FFmpeg config matrix surface to test. Default position: MP3 only; add formats when an agent or user actually asks.
3. **Workspace persistence.** In-memory only (per-session) or disk-cached (sessions span days)? Affects whether `TrackId` returned in session A is valid in session B. Default position: in-memory only for v1; disk cache as a v1.5 follow-up if agent sessions get long enough that re-ingest pain is real.
4. **Cache eviction.** Content-addressed cache will grow forever. LRU from day one or wait? Default position: ignore until it bites; cqs's pattern is "fix it when it actually matters."
5. **Sample-rate normalization.** Force everything to 48 kHz at ingest, or honor source rate? The screw varispeed math is simpler if everything's 48 kHz internally. Default position: normalize to 48 kHz internal. Source rates above 48 kHz get downsampled (`libsoxr VHQ`); below 48 kHz get upsampled. The agent never sees rates other than 48 kHz.
6. **Crossfade curve.** Equal-power vs linear vs cosine. Default position: equal-power (`sin/cos`); ship without exposing the choice as a parameter.
7. **Multi-track overlay.** Can a region be "vocal_acapella over instrumental_slowed"? Adds complexity. Default position: concat-only in v1; overlay lives in v2 alongside the live playback layer if needed. Most screw tapes don't need overlay; the workflow is sequential.
8. **Slow-down algorithm.** Pure resample (varispeed) is the canonical screw vibe. Should `slow_down` always use varispeed, or expose `time_stretch` separately as a non-default option? Default position: `slow_down` is varispeed-only (resampling); a separate `time_stretch` primitive exists for the rare case the agent wants pitch-preserving slow.

## What this is not solving

- No automated key matching across tracks
- No auto-EQ matching, no auto-LUFS leveling between tracks before mix
- No streaming output (render writes a complete file)
- No interruption / partial render — render is atomic
- No live performance in v1 (deferred to v2)
- No DJ-controller integration (different shape of tool entirely)

These are deliberate v1 boundaries. Each one gets its own justification on its own merits if it ever ships.

## Ready-to-build checklist

Before code starts:
- [ ] Audit this document with the user; settle the "Open" list above
- [ ] `tools/screw-mcp/Cargo.toml` skeleton + workspace member registration in `cqs/Cargo.toml`
- [ ] Decide on `aubio-rs` vs hand-rolled tempo/beat (aubio is the default unless build-system pain emerges)
- [ ] Sketch the actual `EditOp` enum and the `samples()` streaming reader trait — these are the architectural primitives that ripple through everything

After audit, the first commit is the workspace member registration + an empty MCP server that returns `{}` to every tool call. From there, phases 0-4 are the build path.
