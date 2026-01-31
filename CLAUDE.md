# CLAUDE.md

cq - semantic code search with local embeddings

## First Run

If `docs/` doesn't exist or any listed files are missing, create them from the templates at the bottom of this document.

## Read Before Doing Anything

* `cq-design-v6.md` -- **source of truth**: architecture, API signatures, verified crate versions
* `docs/SESSION_CONTEXT.md` -- who we are, how we work, conventions
* `docs/HUNCHES.md` -- soft observations, gut feelings, latent risks (append as they arise)
* `ROADMAP.md` -- current progress, what's done, what's next

As audits/reviews happen, add them here:
* `docs/AUDIT_<date>.md` -- audit findings and resolutions

## Tears (Session Continuity)

* `PROJECT_CONTINUITY_<timestamp>.md` -- current state, blockers, next steps (read to resume)
* `PROJECT_CONTINUITY_ARCHIVE_<timestamp>.md` -- session logs, detailed notes (reference only)

Timestamps: UTC, format `YYYY-MM-DDTHHMM[Z]`

**Protocol:**
1. Session start: read tear files before doing anything
2. During work: note decisions, blockers, changes
3. Session end or milestone: update both files
4. Proactively offer updates—don't wait to be asked
5. Flag stale or inconsistent state

## WSL Workarounds

- **Git push**: `powershell.exe -Command "cd C:\projects\cq; git push"` — Windows has GitHub credentials
- **Cargo build**: `.cargo/config.toml` routes target-dir to native Linux path (avoids permission errors on `/mnt/c/`)

## Bootstrap (First Session)

1. Create `docs/` directory
2. Create initial docs from templates below
3. Create initial tear files
4. Scaffold project: `cargo init`, set up Cargo.toml per design doc
5. Create GitHub repo: `gh repo create cq --public --source=. --push`
6. Claim crate name on crates.io (placeholder with intent)

---

## Doc Templates

### docs/SESSION_CONTEXT.md

```markdown
# Session Context

## Communication Style

- Flat, dry, direct
- No warmth padding, enthusiasm, or hedging
- Good questions over wrong answers—ask for context rather than guessing
- Push back when warranted
- Flag assumptions, admit ignorance, own errors without defending

## Expertise Level

- Experienced dev, familiar with Rust ownership/lifetimes
- Don't over-explain basics

## Project Conventions

- Rust edition 2021
- `thiserror` for library errors, `anyhow` in CLI
- `impl Into<PathBuf>` over concrete path types
- No `unwrap()` except in tests
- Streaming/iterator patterns for large result sets
- GPU detection at runtime, graceful CPU fallback

## Tech Stack

- tree-sitter 0.26 (multi-language parsing)
- ort 2.x (ONNX Runtime) - uses `try_extract_array`, `axis_iter`
- tokenizers 0.22
- hf-hub 0.4
- rusqlite 0.31
- nomic-embed-text-v1.5 (768-dim, Matryoshka truncatable)

## Phase 1 Languages

Rust, Python, TypeScript, JavaScript, Go

## Environment

- Claude Code via WSL
- Windows files at `/mnt/c/`
- Tools: `gh` CLI, `cargo`, Rust toolchain
- A6000 GPU (48GB VRAM) for CUDA testing
```

### docs/HUNCHES.md

```markdown
# Hunches

Soft observations, gut feelings, latent risks. Append new entries as they arise.

---

## <date> - <topic>

<observation>

---
```

### ROADMAP.md

```markdown
# Roadmap

## Current Phase: 1 (MVP)

### Done

- [ ] Parser - tree-sitter extraction, all 5 languages
- [ ] Embedder - ort + tokenizers, CUDA/CPU detection, model download
- [ ] Store - sqlite with WAL, BLOB embeddings, brute-force search
- [ ] CLI - init, doctor, index, query, stats, --lang filter
- [ ] Eval - 10 queries per language, measure recall

### Exit Criteria

- `cargo install cq` works
- GPU used when available, CPU fallback works
- 8/10 test queries return relevant results per language
- Index survives Ctrl+C during indexing

## Phase 2: Polish

- More chunk types (classes, structs, interfaces)
- More languages (C, C++, Java, Ruby)
- Path filtering
- Hybrid search (embedding + name match)
- Watch mode
- Stale file detection in doctor

## Phase 3: Integration

- MCP tool for Claude Code
- `--context N` for surrounding code
- VS Code extension

## Phase 4: Scale

- HNSW index for >50k chunks
- Incremental embedding updates
- Index sharing (team sync)
```

### PROJECT_CONTINUITY_<timestamp>.md (Tear)

```markdown
# cq - Project Continuity

Updated: <date>

## Current State

<what works, what doesn't, where we are>

## Recent Changes

<last session's work>

## Blockers / Open Questions

<anything stuck>

## Next Steps

<prioritized list>

## Decisions Made

<key choices with brief rationale>
```

### PROJECT_CONTINUITY_ARCHIVE_<timestamp>.md

```markdown
# cq - Archive

Session log and detailed notes.

---

## Session: <date>

### <topic>

<detailed notes, code snippets, error messages, research>

---
```

---

## Why "Tears"

Etymology: PIE *teks- (weave/construct). Also collapses with *der- (rip) and *dakru- (crying). Construction, destruction, loss—the full arc of session boundaries.
