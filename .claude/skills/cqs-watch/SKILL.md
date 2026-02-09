---
name: cqs-watch
description: Start the cqs file watcher to keep the index fresh automatically.
disable-model-invocation: false
argument-hint: "[--debounce 500] [--no-ignore]"
---

# Watch Mode

Start the cqs file watcher to keep the index updated as files change.

## Usage

Run: `cqs watch [--debounce <ms>] [--no-ignore]`

- `--debounce <ms>` — Debounce interval in milliseconds (default: 500). How long to wait after a file change before re-indexing.
- `--no-ignore` — Don't respect `.gitignore` rules. Index all files including those normally ignored.

## Behavior

- Watches the project directory recursively for file changes
- Re-indexes changed files automatically (incremental — only changed files)
- Respects `.gitignore` by default
- Debounces rapid changes to avoid re-indexing on every keystroke
- Runs in foreground — use a separate terminal or background it

## When to use

- During active development to keep search results current
- Before relying on `cqs search` results if you've made recent changes
- Not needed if you manually run `cqs index` when needed

## Alternative

If you just need a one-time refresh: `cqs index` (or `/reindex` for before/after stats).
