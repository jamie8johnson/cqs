---
name: troubleshoot
description: Diagnose common cqs issues — stale index, MCP not connecting, model download, schema mismatch.
disable-model-invocation: false
argument-hint: ""
---

# Troubleshoot

Guided diagnosis of common cqs issues. Run through checks in order, stop at first problem found.

## Checklist

### 1. Is cqs installed and working?

```bash
cqs --version
```

If not found: `cargo install --path .` (from source) or check `~/.cargo/bin/` is in PATH.

### 2. Is the project initialized?

```bash
ls -la .cq/
```

Should contain `index.db` and `hnsw.bin`. If missing: `cqs init && cqs index`.

### 3. Is the index populated?

Call `cqs_stats` MCP tool (or `cqs stats`). Check:
- Chunk count > 0
- Last update is recent
- Expected languages are present

If empty: `cqs index` to rebuild.

### 4. Schema version mismatch?

```bash
cqs stats 2>&1
```

If you see "SchemaMismatch" or "SchemaNewerThanCq":
- **Older schema**: Run `/migrate` to upgrade
- **Newer schema**: Update cqs binary to latest version

Current schema version: check `src/store/helpers.rs` for `CURRENT_SCHEMA_VERSION`.

### 5. Model downloaded?

```bash
ls -la ~/.cache/huggingface/hub/models--intfloat--e5-base-v2/
```

If missing or incomplete, cqs downloads on first use. Check network access to huggingface.co.
If corrupted: delete the directory and let cqs re-download (blake3 checksums verify integrity).

### 6. MCP server connecting?

```bash
cqs serve --stdio 2>/dev/null
```

Should start without error. Common issues:
- Port conflict (HTTP mode): another process on the port
- API key mismatch: check `CQS_API_KEY` env var or `--api-key-file`
- Check Claude Code MCP config: `.claude/mcp.json` or global `~/.claude/mcp.json`

### 7. Index stale?

Compare `cqs stats` last update time vs recent file modifications.
Fix: `cqs index` (one-time) or `cqs watch` (continuous).

### 8. References broken?

```bash
cqs ref list
```

Check that source paths still exist and chunk counts are > 0.
If source moved: `cqs ref remove <name>` and re-add with new path.

## Report

After running checks, summarize:
- What was checked
- What failed (if anything)
- What was fixed or needs fixing
