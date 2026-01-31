# Hunches

Soft observations, gut feelings, latent risks. Append new entries as they arise.

---

## 2026-01-31 - tree-sitter version gap

Grammar crates (0.23.x) have dev-dep on tree-sitter ^0.23, but we're using tree-sitter 0.26. Works via `tree-sitter-language` abstraction layer, but feels fragile. If parsing breaks mysteriously, check this first.

---

## 2026-01-31 - ort 2.x is still RC

Using `ort = "2.0.0-rc.11"` - no stable 2.0 release yet. API could change. Pin exact version and watch for breaking changes on upgrade.

---

## 2026-01-31 - WSL /mnt/c/ permission hell

Building Rust on Windows paths from WSL causes random permission errors (libsqlite3-sys, git config). Workaround in place (.cargo/config.toml), but might bite us elsewhere.

---
