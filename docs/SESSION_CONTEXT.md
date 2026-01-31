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
