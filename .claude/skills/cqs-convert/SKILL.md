---
name: cqs-convert
description: Convert documents (PDF, HTML, CHM, web help, Markdown) to cleaned, renamed Markdown files for indexing.
disable-model-invocation: false
argument-hint: "<path> [--output <dir>] [--overwrite] [--dry-run] [--clean-tags <tags>]"
---

# Convert

Convert documents to cleaned Markdown with sensible kebab-case filenames.

## Supported Formats

| Format | Engine | Requirements |
|--------|--------|-------------|
| PDF | Python pymupdf4llm | `python3`, `pip install pymupdf4llm` |
| HTML/HTM | Rust fast_html2md | None |
| CHM | 7z + fast_html2md | `sudo apt install p7zip-full` |
| Web Help | fast_html2md (multi-page) | None (auto-detected by `content/` subdir) |
| Markdown | Passthrough | None (cleaning + renaming only) |

## Arguments

- First positional arg = file or directory path (required)
- `--output <dir>` → output directory for .md files (default: same as input)
- `--overwrite` → overwrite existing .md files
- `--dry-run` → preview conversions without writing
- `--clean-tags <tags>` → comma-separated cleaning rule tags (default: all rules)

## Cleaning Tags

- `aveva` — AVEVA-specific artifacts (copyright blocks, page boundaries)
- `pdf` — PDF conversion artifacts (TOC, chapter headings)
- `generic` — universal cleanup (bold bullets, strikethrough, blank line collapse)

Run via Bash: `cqs convert <path> [flags]`

## Examples

- `/cqs-convert samples/pdf/` — convert all PDFs in directory
- `/cqs-convert doc.html --output converted/` — convert single HTML file
- `/cqs-convert samples/pdf/ --output samples/converted/ --dry-run` — preview batch
- `/cqs-convert raw.md --output cleaned/` — clean and rename a markdown file
- `/cqs-convert samples/web/ --output converted/` — auto-detect and merge web help sites
