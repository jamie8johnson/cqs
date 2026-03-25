From v1.5.0 audit:

- CQ-25: ~2000 lines of LLM-generated `# Arguments`/`# Returns` doc comments on trivial functions (builder methods, thin dispatch wrappers). Not a bug — note for improving doc generation heuristics to skip trivial functions.
- PB-24: `prune_missing` compares `PathBuf::from(origin)` with canonicalized `existing_files` — can miss on case-insensitive filesystems (macOS HFS+/APFS). Linux/WSL unaffected.
- EH-29: `read_context_lines` errors silently dropped — correct behavior for display fallback, no diagnostic needed.
- EH-30: Bm25 `top_k_negatives` can return empty docs as hard negatives — training data only, low impact.
