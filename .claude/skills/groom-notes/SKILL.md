---
name: groom-notes
description: Review and clean up stale notes in notes.toml. Use when notes accumulate and need pruning.
disable-model-invocation: false
argument-hint: "[--warnings|--patterns|--all]"
---

# Groom Notes

Review all notes in `docs/notes.toml` and interactively clean up stale, outdated, or redundant ones.

## Process

1. **Parse notes**: Read `docs/notes.toml` and list all notes with their sentiment, text preview, and mentions.

2. **Identify candidates for removal**:
   - Notes that reference removed/renamed files (check with `Glob`)
   - Notes that contradict current code (e.g., "X doesn't work" when X was fixed)
   - Duplicate or near-duplicate notes covering the same topic
   - Notes about temporary issues that have been resolved
   - Notes with stale mentions (files that no longer exist)

3. **Present findings**: Show a summary of candidates grouped by reason:
   - **Stale**: References removed code or fixed issues
   - **Superseded**: Newer note covers the same ground
   - **Duplicate**: Multiple notes saying essentially the same thing

4. **Get approval**: Present the list of proposed removals. Ask the user to confirm before making changes.

5. **Execute**: Use `cqs_remove_note` MCP tool for each confirmed removal. If the MCP server isn't running, edit `docs/notes.toml` directly using `rewrite_notes_file` pattern (atomic write).

6. **Report**: Show before/after count and what was removed.

## Staleness checks

For each note, verify mentions still exist:
```
Glob for each file in mentions[] — if none match, flag as potentially stale
```

Cross-reference with git log — if a note mentions an issue number, check if that issue is closed.

## Rules

- Never remove notes without user approval
- Preserve the file header comments
- Keep notes that capture general lessons even if the specific code changed
- When in doubt, keep the note — false negatives (missing a useful note) are worse than false positives (keeping a stale one)
- After grooming, suggest running `cqs index` to update the search index
